//! Local Whisper transcription through the official whisper.cpp CLI.
//!
//! The engine and GGML models are downloaded on demand into the app data
//! directory, verified against pinned SHA-256 hashes, and executed per 8
//! minute audio segment. Audio never leaves the machine.

use crate::asr::{self, AudioChunkRef, Transcript, TranscriptSegment};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

pub const DEFAULT_MODEL: &str = "small";
const ENGINE_TAG: &str = "b5454";
const ENGINE_FILE: &str = "whisper-cli.exe";
const MODEL_DIR: &str = "models";
const WAV_SEGMENT_SECONDS: u64 = 480;

struct EngineSpec {
    id: &'static str,
    label: &'static str,
    url: &'static str,
    size_bytes: u64,
    sha256: &'static str,
    dir: &'static str,
}

const ENGINES: &[EngineSpec] = &[
    EngineSpec {
        id: "cpu",
        label: "CPU 引擎（约 9 MB）",
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip",
        size_bytes: 8_928_640,
        sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
        dir: "engine",
    },
    EngineSpec {
        id: "cuda",
        label: "CUDA 引擎（约 272 MB）",
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-win-cuda-11.8.0-x64.zip",
        size_bytes: 284_761_231,
        sha256: "e81c86c542be6e3d7b7245392d4e1c52ab6e5c61dbe38721754bc0ab59d4d027",
        dir: "engine-cuda",
    },
];

struct ModelSpec {
    id: &'static str,
    label: &'static str,
    size_bytes: u64,
    sha256: &'static str,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "tiny",
        label: "Tiny（实时字幕）",
        size_bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    ModelSpec {
        id: "base",
        label: "Base（快）",
        size_bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    ModelSpec {
        id: "small",
        label: "Small（推荐）",
        size_bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    ModelSpec {
        id: "large-v3-turbo-q5_0",
        label: "Large v3 Turbo（高质量·量化）",
        size_bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
];

/// Serializes engine and model downloads so only one runs at a time.
#[derive(Default)]
pub struct LocalWhisperState(Mutex<Option<String>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModelStatus {
    id: String,
    label: String,
    size_bytes: u64,
    installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    id: String,
    label: String,
    size_bytes: u64,
    installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWhisperStatus {
    engine_installed: bool,
    engine_bytes: u64,
    engine_version: &'static str,
    engine: String,
    engines: Vec<EngineStatus>,
    gpu_name: Option<String>,
    model: String,
    model_installed: bool,
    models: Vec<WhisperModelStatus>,
    total_bytes: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    kind: &'static str,
    model: Option<String>,
    downloaded: u64,
    total: u64,
}

pub fn is_known_model(id: &str) -> bool {
    MODELS.iter().any(|model| model.id == id)
}

fn model_spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|model| model.id == id)
}

fn root(app_data: &Path) -> PathBuf {
    app_data.join("local-whisper")
}

fn engine_spec(id: &str) -> Option<&'static EngineSpec> {
    ENGINES.iter().find(|engine| engine.id == id)
}

fn engine_dir(app_data: &Path, spec: &EngineSpec) -> PathBuf {
    root(app_data).join(spec.dir)
}

fn engine_exe(app_data: &Path, spec: &EngineSpec) -> PathBuf {
    engine_dir(app_data, spec).join(ENGINE_FILE)
}

/// The CLI is useless without its runtime DLLs, so treat a partial
/// extraction as not installed.
fn engine_ready(app_data: &Path, spec: &EngineSpec) -> bool {
    let directory = engine_dir(app_data, spec);
    directory.join(ENGINE_FILE).is_file()
        && directory.join("whisper.dll").is_file()
        && directory.join("ggml.dll").is_file()
}

/// Prefers the CUDA engine when installed and a GPU is present, otherwise CPU.
fn active_engine(app_data: &Path) -> Option<&'static EngineSpec> {
    if gpu_name().is_some()
        && let Some(cuda) = ENGINES
            .iter()
            .find(|spec| spec.id == "cuda" && engine_ready(app_data, spec))
    {
        return Some(cuda);
    }
    ENGINES
        .iter()
        .find(|spec| spec.id == "cpu" && engine_ready(app_data, spec))
}

/// Cached GPU detection: hardware does not change while the app runs, and the
/// live path consults this on every poll.
fn gpu_name() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(detect_gpu).clone()
}

/// Detects an NVIDIA GPU through `nvidia-smi`; CUDA builds need one.
fn detect_gpu() -> Option<String> {
    let mut command = Command::new("nvidia-smi");
    command.args(["--query-gpu=name", "--format=csv,noheader"]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_owned();
    (!name.is_empty()).then_some(name)
}

fn models_dir(app_data: &Path) -> PathBuf {
    root(app_data).join(MODEL_DIR)
}

fn model_path(app_data: &Path, id: &str) -> PathBuf {
    models_dir(app_data).join(format!("ggml-{id}.bin"))
}

fn model_url(id: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{id}.bin")
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))
}

fn engine_bytes(app_data: &Path, spec: &EngineSpec) -> u64 {
    let directory = engine_dir(app_data, spec);
    let Ok(entries) = fs::read_dir(&directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

fn status_for(app_data: &Path, selected: &str) -> LocalWhisperStatus {
    let active = active_engine(app_data);
    let mut total_bytes = 0_u64;
    let engines = ENGINES
        .iter()
        .map(|spec| {
            let installed = engine_ready(app_data, spec);
            let bytes = engine_bytes(app_data, spec);
            if installed {
                total_bytes += bytes;
            }
            EngineStatus {
                id: spec.id.to_owned(),
                label: spec.label.to_owned(),
                size_bytes: spec.size_bytes,
                installed,
            }
        })
        .collect::<Vec<_>>();
    let model = if is_known_model(selected) {
        selected.to_owned()
    } else {
        DEFAULT_MODEL.to_owned()
    };
    let models = MODELS
        .iter()
        .map(|spec| {
            let installed = model_path(app_data, spec.id).is_file();
            if installed {
                total_bytes += spec.size_bytes;
            }
            WhisperModelStatus {
                id: spec.id.to_owned(),
                label: spec.label.to_owned(),
                size_bytes: spec.size_bytes,
                installed,
            }
        })
        .collect();
    LocalWhisperStatus {
        engine_installed: active.is_some(),
        engine_bytes: ENGINES
            .iter()
            .map(|spec| engine_bytes(app_data, spec))
            .sum(),
        engine_version: ENGINE_TAG,
        engine: active.map(|spec| spec.id.to_owned()).unwrap_or_default(),
        engines,
        gpu_name: gpu_name(),
        model_installed: model_path(app_data, &model).is_file(),
        model,
        models,
        total_bytes,
    }
}

fn selected_model(settings: &crate::AppSettings) -> String {
    if is_known_model(&settings.local_whisper_model) {
        settings.local_whisper_model.clone()
    } else {
        DEFAULT_MODEL.to_owned()
    }
}

fn local_settings(settings: &tauri::State<'_, Mutex<crate::AppSettings>>) -> crate::AppSettings {
    settings.lock().expect("settings state poisoned").clone()
}

#[tauri::command]
pub fn get_local_whisper_status(
    app: AppHandle,
    settings: tauri::State<'_, Mutex<crate::AppSettings>>,
) -> Result<LocalWhisperStatus, String> {
    let app_data = app_data_dir(&app)?;
    let selected = selected_model(&local_settings(&settings));
    Ok(status_for(&app_data, &selected))
}

#[tauri::command]
pub async fn download_local_whisper_engine(
    variant: Option<String>,
    app: AppHandle,
    state: tauri::State<'_, LocalWhisperState>,
    settings: tauri::State<'_, Mutex<crate::AppSettings>>,
) -> Result<LocalWhisperStatus, String> {
    let spec = engine_spec(variant.as_deref().unwrap_or("cpu"))
        .ok_or_else(|| "未知的本地 Whisper 引擎类型".to_owned())?;
    let _guard = begin_download(&state, spec.id)?;
    let app_data = app_data_dir(&app)?;
    let engine = engine_dir(&app_data, spec);
    if engine_ready(&app_data, spec) {
        return Ok(status_for(
            &app_data,
            &selected_model(&local_settings(&settings)),
        ));
    }
    if spec.id == "cuda" && gpu_name().is_none() {
        return Err("未检测到 NVIDIA GPU，无法使用 CUDA 引擎".into());
    }
    let archive = root(&app_data).join(format!("whisper-bin-{}.zip", spec.id));
    fs::create_dir_all(root(&app_data)).map_err(|error| error.to_string())?;
    crate::logging::info(
        "whisper",
        &format!("downloading {} engine ({} bytes)", spec.id, spec.size_bytes),
    );
    if let Err(error) = download_file(
        &app,
        spec.url,
        &archive,
        spec.size_bytes,
        spec.sha256,
        "engine",
        None,
    )
    .await
    {
        crate::logging::error(
            "whisper",
            &format!("{} engine download failed: {error}", spec.id),
        );
        return Err(error);
    }
    let extracted = extract_engine(&archive, &engine);
    let _ = fs::remove_file(&archive);
    extracted?;
    if !engine.join(ENGINE_FILE).is_file() {
        return Err(format!("解压后未找到 {ENGINE_FILE}"));
    }
    crate::logging::info("whisper", &format!("{} engine installed", spec.id));
    Ok(status_for(
        &app_data,
        &selected_model(&local_settings(&settings)),
    ))
}

#[tauri::command]
pub async fn download_local_whisper_model(
    model: String,
    app: AppHandle,
    state: tauri::State<'_, LocalWhisperState>,
    settings: tauri::State<'_, Mutex<crate::AppSettings>>,
) -> Result<LocalWhisperStatus, String> {
    let spec = model_spec(&model).ok_or_else(|| "未知的本地 Whisper 模型".to_owned())?;
    let _guard = begin_download(&state, &format!("model:{}", spec.id))?;
    let app_data = app_data_dir(&app)?;
    let selected = selected_model(&local_settings(&settings));
    let destination = model_path(&app_data, spec.id);
    if destination.is_file() {
        return Ok(status_for(&app_data, &selected));
    }
    fs::create_dir_all(models_dir(&app_data)).map_err(|error| error.to_string())?;
    let url = model_url(spec.id);
    crate::logging::info(
        "whisper",
        &format!("downloading model {} ({} bytes)", spec.id, spec.size_bytes),
    );
    if let Err(error) = download_file(
        &app,
        &url,
        &destination,
        spec.size_bytes,
        spec.sha256,
        "model",
        Some(spec.id.to_owned()),
    )
    .await
    {
        crate::logging::error(
            "whisper",
            &format!("model {} download failed: {error}", spec.id),
        );
        return Err(error);
    }
    crate::logging::info("whisper", &format!("model {} installed", spec.id));
    Ok(status_for(&app_data, &selected))
}

#[tauri::command]
pub fn delete_local_whisper_model(
    model: String,
    app: AppHandle,
    settings: tauri::State<'_, Mutex<crate::AppSettings>>,
) -> Result<LocalWhisperStatus, String> {
    let spec = model_spec(&model).ok_or_else(|| "未知的本地 Whisper 模型".to_owned())?;
    let app_data = app_data_dir(&app)?;
    let path = model_path(&app_data, spec.id);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("无法删除模型：{error}")),
    }
    Ok(status_for(
        &app_data,
        &selected_model(&local_settings(&settings)),
    ))
}

fn begin_download<'a>(
    state: &'a LocalWhisperState,
    key: &str,
) -> Result<DownloadGuard<'a>, String> {
    let mut slot = state.0.lock().expect("local whisper state poisoned");
    if let Some(active) = slot.as_ref() {
        return Err(format!("已有本地 Whisper 下载任务：{active}"));
    }
    *slot = Some(key.to_owned());
    Ok(DownloadGuard(&state.0))
}

struct DownloadGuard<'a>(&'a Mutex<Option<String>>);

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().expect("local whisper state poisoned") = None;
    }
}

async fn download_file(
    app: &AppHandle,
    url: &str,
    destination: &Path,
    expected_size: u64,
    expected_sha256: &str,
    kind: &'static str,
    model: Option<String>,
) -> Result<(), String> {
    let temporary = destination.with_extension("part");
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("下载失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载失败：HTTP {}", response.status()));
    }
    let total = response.content_length().unwrap_or(expected_size);
    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|error| format!("无法创建下载文件：{error}"))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut reported = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("下载中断：{error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("写入下载文件失败：{error}"))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded.saturating_sub(reported) >= 512 * 1024 || downloaded == total {
            reported = downloaded;
            emit_progress(app, kind, model.clone(), downloaded, total);
        }
    }
    file.flush()
        .await
        .map_err(|error| format!("写入下载文件失败：{error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("写入下载文件失败：{error}"))?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if downloaded != expected_size || !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!(
            "下载文件校验失败：期望 {expected_size} 字节 / {expected_sha256}，实际 {downloaded} 字节 / {actual}"
        ));
    }
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|error| format!("无法保存下载文件：{error}"))?;
    emit_progress(app, kind, model, expected_size, expected_size);
    Ok(())
}

fn emit_progress(
    app: &AppHandle,
    kind: &'static str,
    model: Option<String>,
    downloaded: u64,
    total: u64,
) {
    let _ = app.emit(
        "local-whisper-download",
        DownloadProgress {
            kind,
            model,
            downloaded,
            total,
        },
    );
}

fn extract_engine(archive: &Path, engine: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|error| format!("无法打开下载的引擎：{error}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|error| format!("无法解压本地引擎：{error}"))?;
    fs::create_dir_all(engine).map_err(|error| error.to_string())?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("无法读取压缩包：{error}"))?;
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let mut components = path.components();
        let Some(top) = components.next() else {
            continue;
        };
        if top.as_os_str() != "Release" {
            continue;
        }
        let Some(name) = components.next() else {
            continue;
        };
        if components.next().is_some() || entry.is_dir() {
            continue;
        }
        let target = engine.join(name);
        let mut output =
            File::create(&target).map_err(|error| format!("无法写入引擎文件：{error}"))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("无法解压引擎文件：{error}"))?;
        output
            .sync_all()
            .map_err(|error| format!("无法写入引擎文件：{error}"))?;
    }
    Ok(())
}

/// Options shared by batch and live local transcription.
pub struct LocalOptions<'a> {
    pub model: &'a str,
    pub language: &'a str,
    /// Whisper initial prompt, used for hotwords and punctuation hints.
    pub initial_prompt: &'a str,
}

/// Everything one local transcription run needs besides the audio chunks.
struct LocalJob<'a> {
    working: &'a Path,
    exe: &'a Path,
    model_file: &'a Path,
    spec: &'a ModelSpec,
    language: &'a str,
    initial_prompt: &'a str,
    cancel: &'a AtomicBool,
    on_progress: &'a (dyn Fn(usize, usize) + Sync),
}

/// Transcribes all committed chunks with the local whisper.cpp CLI. Runs on a
/// blocking thread; one 8 minute WAV segment per `whisper-cli` process.
pub fn transcribe(
    app_data: &Path,
    chunks: &[AudioChunkRef],
    session_id: &str,
    options: &LocalOptions<'_>,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Transcript, String> {
    let spec = model_spec(options.model)
        .ok_or_else(|| "未知的本地 Whisper 模型，请在转写设置中重新选择".to_owned())?;
    let Some(engine) = active_engine(app_data) else {
        return Err("本地 Whisper 引擎未安装，请在转写设置中下载".into());
    };
    let exe = engine_exe(app_data, engine);
    let model_file = model_path(app_data, spec.id);
    if !model_file.is_file() {
        return Err(format!("本地 Whisper 模型未下载：{}", spec.label));
    }
    crate::logging::info(
        "whisper",
        &format!(
            "transcribe start engine={} model={} chunks={}",
            engine.id,
            spec.id,
            chunks.len()
        ),
    );
    let working =
        std::env::temp_dir().join(format!("magnetic-tape-whisper-{session_id}-{}", epoch_ms()));
    let job = LocalJob {
        working: &working,
        exe: &exe,
        model_file: &model_file,
        spec,
        language: options.language,
        initial_prompt: options.initial_prompt,
        cancel,
        on_progress,
    };
    let result = transcribe_in(&job, chunks);
    let _ = fs::remove_dir_all(&working);
    result
}

/// Picks the model used for live captions.
///
/// The user picks a model in the settings, so that choice wins whenever it is
/// installed. Falling back to the smallest installed model keeps live captions
/// working after a model was deleted, but it must not silently downgrade a
/// deliberate choice such as `large-v3-turbo-q5_0`.
pub fn pick_live_model(app_data: &Path, preferred: &str) -> Result<String, String> {
    if active_engine(app_data).is_none() {
        return Err("本地 Whisper 引擎未安装，请在转写设置中下载".into());
    }
    if is_known_model(preferred) && model_path(app_data, preferred).is_file() {
        return Ok(preferred.to_owned());
    }
    for id in ["tiny", "base", "small", "large-v3-turbo-q5_0"] {
        if model_path(app_data, id).is_file() {
            return Ok(id.to_owned());
        }
    }
    Err("本地实时字幕需要先下载模型（转写设置 → 本地 Whisper）".into())
}

fn transcribe_in(job: &LocalJob<'_>, chunks: &[AudioChunkRef]) -> Result<Transcript, String> {
    let segments = asr::build_segments(chunks, job.working, WAV_SEGMENT_SECONDS)?;
    if segments.is_empty() {
        return Err("当前会话没有可转写的音频".into());
    }
    let total = segments.len();
    let mut transcript = Transcript {
        provider: "local_whisper".into(),
        model: job.spec.id.into(),
        language: job.language.to_owned(),
        duration_ms: 0,
        text: String::new(),
        segments: Vec::new(),
        created_at_epoch_ms: epoch_ms(),
    };
    let mut failures = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if job.cancel.load(Ordering::SeqCst) {
            return Err(asr::CANCELLED.into());
        }
        let prefix = job.working.join(format!("result-{index:03}"));
        match run_segment(
            job.exe,
            job.model_file,
            segment,
            job.language,
            job.initial_prompt,
            &prefix,
        ) {
            Ok(lines) => {
                transcript.duration_ms = transcript
                    .duration_ms
                    .max(segment.start_ms + segment.duration_ms);
                for (start, end, text) in lines {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if !transcript.text.is_empty() {
                        transcript.text.push('\n');
                    }
                    transcript.text.push_str(text);
                    transcript.segments.push(TranscriptSegment {
                        start_ms: segment.start_ms + start,
                        end_ms: segment.start_ms + end.max(start),
                        speaker: None,
                        text: text.to_owned(),
                    });
                }
            }
            Err(error) => {
                crate::logging::warn("whisper", &format!("segment {} failed: {error}", index + 1));
                failures.push(format!("第 {} 段：{error}", index + 1));
            }
        }
        (job.on_progress)(index + 1, total);
    }
    transcript
        .segments
        .sort_by_key(|segment| (segment.start_ms, segment.end_ms));
    if transcript.segments.is_empty() && !failures.is_empty() {
        return Err(failures.join("；"));
    }
    Ok(transcript)
}

fn run_segment(
    exe: &Path,
    model: &Path,
    segment: &asr::AudioSegment,
    language: &str,
    initial_prompt: &str,
    prefix: &Path,
) -> Result<Vec<(u64, u64, String)>, String> {
    let mut command = Command::new(exe);
    command
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(&segment.path)
        .arg("-l")
        .arg(whisper_language(language));
    if !initial_prompt.trim().is_empty() {
        // whisper-cli only exposes the long form; "-p" is not a valid flag and
        // makes this build abort.
        command.arg("--prompt").arg(initial_prompt.trim());
    }
    command
        .arg("-oj")
        .arg("-of")
        .arg(prefix)
        .arg("-nt")
        .arg("-np")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动本地转写引擎：{error}"))?;
    let stderr = child.stderr.take().map(spawn_stderr_reader);
    let timeout = Duration::from_secs((segment.duration_ms / 1_000).saturating_mul(4).max(180));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(format!("无法等待本地转写引擎：{error}"));
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("本地转写超时".into());
        }
        thread::sleep(Duration::from_millis(200));
    };
    let stderr_text = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    if !status.success() {
        let detail: String = stderr_text.chars().take(500).collect();
        return Err(format!("whisper-cli 退出码 {:?}：{detail}", status.code()));
    }
    let json_path = prefix.with_extension("json");
    parse_output(&json_path)
}

fn spawn_stderr_reader(mut reader: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if collected.len() < 8_192 {
                        let remaining = 8_192 - collected.len();
                        collected.extend_from_slice(&buffer[..count.min(remaining)]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&collected).trim().to_owned()
    })
}

#[derive(Deserialize)]
struct WhisperOutput {
    #[serde(default)]
    transcription: Vec<WhisperLine>,
}

#[derive(Deserialize)]
struct WhisperLine {
    offsets: WhisperOffsets,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct WhisperOffsets {
    from: u64,
    to: u64,
}

fn parse_output(path: &Path) -> Result<Vec<(u64, u64, String)>, String> {
    let bytes = fs::read(path).map_err(|error| format!("无法读取转写结果：{error}"))?;
    let output: WhisperOutput =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析转写结果：{error}"))?;
    Ok(output
        .transcription
        .into_iter()
        .map(|line| (line.offsets.from, line.offsets.to, line.text))
        .collect())
}

fn whisper_language(language: &str) -> &str {
    match language {
        "" | "auto" => "auto",
        "zh-CN" => "zh",
        "en-US" => "en",
        other => other,
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_catalog_is_stable_and_safe() {
        assert!(is_known_model("small"));
        assert!(is_known_model("tiny"));
        assert!(is_known_model("base"));
        assert!(is_known_model("large-v3-turbo-q5_0"));
        assert!(!is_known_model("../evil"));
        assert!(!is_known_model("ggml-small"));
        assert_eq!(DEFAULT_MODEL, "small");
        for model in MODELS {
            assert!(model.size_bytes > 0);
            assert_eq!(model.sha256.len(), 64);
            assert!(!model.id.contains('/'));
            assert!(!model.id.contains(".."));
        }
    }

    #[test]
    fn model_paths_stay_inside_the_model_directory() {
        let app_data = Path::new("C:/data");
        let path = model_path(app_data, "small");
        assert!(path.ends_with("local-whisper/models/ggml-small.bin"));
    }

    #[test]
    fn engine_catalog_uses_separate_directories() {
        let cpu = engine_spec("cpu").unwrap();
        let cuda = engine_spec("cuda").unwrap();
        assert!(engine_spec("../cpu").is_none());
        assert_eq!(cpu.dir, "engine");
        assert_eq!(cuda.dir, "engine-cuda");
        let app_data = Path::new("C:/data");
        assert!(engine_dir(app_data, cpu).ends_with("local-whisper/engine"));
        assert!(engine_dir(app_data, cuda).ends_with("local-whisper/engine-cuda"));
        assert_eq!(engine_exe(app_data, cuda).file_name().unwrap(), ENGINE_FILE);
    }

    #[test]
    fn maps_languages_to_whisper_codes() {
        assert_eq!(whisper_language("auto"), "auto");
        assert_eq!(whisper_language("zh-CN"), "zh");
        assert_eq!(whisper_language("en-US"), "en");
        assert_eq!(whisper_language("ja"), "ja");
    }

    #[test]
    fn parses_whisper_json_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.json");
        fs::write(
            &path,
            r#"{"transcription":[{"offsets":{"from":0,"to":1500},"text":" 大家好。"},{"offsets":{"from":1500,"to":3200},"text":"开始吧。"}]}"#,
        )
        .unwrap();
        let lines = parse_output(&path).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], (0, 1_500, " 大家好。".into()));
        assert_eq!(lines[1].1, 3_200);
    }

    /// Manual smoke test for the pinned engine archive.
    /// Run with `WHISPER_ARCHIVE` set to the downloaded zip, plus `--ignored`.
    #[test]
    #[ignore = "requires WHISPER_ARCHIVE pointing at whisper-bin-x64.zip"]
    fn extracts_the_downloaded_engine_archive() {
        let Ok(archive) = std::env::var("WHISPER_ARCHIVE") else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let engine = directory.path().join("engine");
        extract_engine(Path::new(&archive), &engine).unwrap();
        assert!(engine.join("whisper-cli.exe").is_file());
        assert!(engine.join("whisper.dll").is_file());
        assert!(engine.join("ggml.dll").is_file());
        assert!(!engine.join("Release").exists());
    }

    /// Manual smoke test against a real downloaded engine and model.
    /// Run with `WHISPER_ENGINE_DIR` and `WHISPER_MODEL` set, plus `--ignored`.
    #[test]
    #[ignore = "requires WHISPER_ENGINE_DIR and WHISPER_MODEL"]
    fn transcribes_a_real_chunk_with_the_downloaded_engine() {
        let Ok(engine_source) = std::env::var("WHISPER_ENGINE_DIR") else {
            return;
        };
        let Ok(model_source) = std::env::var("WHISPER_MODEL") else {
            return;
        };
        let app_data = tempfile::tempdir().unwrap();
        let engine = app_data.path().join("local-whisper/engine");
        let models = app_data.path().join("local-whisper/models");
        fs::create_dir_all(&engine).unwrap();
        fs::create_dir_all(&models).unwrap();
        for entry in fs::read_dir(&engine_source).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_file() {
                fs::copy(entry.path(), engine.join(entry.file_name())).unwrap();
            }
        }
        fs::copy(&model_source, models.join("ggml-base.bin")).unwrap();
        let chunk = AudioChunkRef {
            track: magnetic_tape_audio::Track::Microphone,
            path: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../doc/chunk_00000.ogg"),
            start_frame: 0,
            frames: 144_000,
        };
        let prompt = std::env::var("WHISPER_PROMPT").unwrap_or_default();
        let options = LocalOptions {
            model: "base",
            language: "auto",
            initial_prompt: if prompt.is_empty() {
                ""
            } else {
                prompt.as_str()
            },
        };
        let transcript = transcribe(
            app_data.path(),
            &[chunk],
            "smoke",
            &options,
            &AtomicBool::new(false),
            &|_, _| {},
        )
        .unwrap();
        if prompt.is_empty() {
            assert!(!transcript.text.trim().is_empty());
            assert!(!transcript.segments.is_empty());
        }
    }
}
