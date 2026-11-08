//! Local streaming speech recognition and speaker embeddings through
//! sherpa-onnx.
//!
//! The engine is downloaded on demand into the app data directory and loaded
//! with `LoadLibraryExW`, so the build has no libclang or C toolchain
//! requirement. Audio never leaves the machine.
//!
//! Live captions consume the same 24 kHz mixed PCM stream that the OpenAI
//! Realtime path uses ([`magnetic_tape_audio::AudioRecording::drain_realtime_pcm`]),
//! so one recognizer replaces the five-second chunk polling loop. Speaker
//! labels come from a per-segment embedding compared against a running
//! centroid gallery.

use crate::asr::TranscriptSegment;
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

const ENGINE_VERSION: &str = "1.12.9";
const ENGINE_FILE: &str = "sherpa-onnx-c-api.dll";
const ENGINE_RUNTIME: &str = "onnxruntime.dll";
const ENGINE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.9/sherpa-onnx-v1.12.9-win-x64-shared.tar.bz2";
const ENGINE_SIZE: u64 = 24_139_676;
const ENGINE_SHA: &str = "57a0dff63c468ed067c6ffd1a9c9b6f66f4cf73304d44b5f17518010ce2e0977";

const ASR_ID: &str = "zh-14m";
const ASR_LABEL: &str = "中文流式 Zipformer 14M";
const ASR_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23.tar.bz2";
const ASR_SIZE: u64 = 74_004_050;

const SPEAKER_FILE: &str = "3dspeaker_campplus_zh_cn.onnx";
const SPEAKER_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx";
const SPEAKER_SIZE: u64 = 28_281_138;

const DIARIZATION_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
const DIARIZATION_SIZE: u64 = 6_958_444;

const SAMPLE_RATE: i32 = 24_000;
const ASR_SAMPLE_RATE: i32 = 16_000;
/// Cosine similarity above which an embedding joins an existing speaker.
const SPEAKER_THRESHOLD: f32 = 0.65;
/// Upper bound on distinct speakers; extra voices fold into the nearest one.
const MAX_SPEAKERS: usize = 8;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    kind: &'static str,
    downloaded: u64,
    total: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SherpaStatus {
    engine_installed: bool,
    engine_bytes: u64,
    engine_version: &'static str,
    asr_model_installed: bool,
    asr_model: &'static str,
    asr_model_label: &'static str,
    asr_model_bytes: u64,
    speaker_model_installed: bool,
    speaker_model_bytes: u64,
    diarization_installed: bool,
    diarization_model_bytes: u64,
    total_bytes: u64,
    ready: bool,
}

#[derive(Default)]
pub struct SherpaDownloadState(Mutex<Option<String>>);

fn root(app_data: &Path) -> PathBuf {
    app_data.join("sherpa")
}

fn engine_dir(app_data: &Path) -> PathBuf {
    root(app_data).join("engine")
}

fn models_dir(app_data: &Path) -> PathBuf {
    root(app_data).join("models")
}

fn asr_dir(app_data: &Path) -> PathBuf {
    models_dir(app_data).join(ASR_ID)
}

fn speaker_path(app_data: &Path) -> PathBuf {
    models_dir(app_data).join(SPEAKER_FILE)
}

fn diarization_dir(app_data: &Path) -> PathBuf {
    models_dir(app_data).join("pyannote-3.0")
}

pub fn engine_ready(app_data: &Path) -> bool {
    let engine = engine_dir(app_data);
    engine.join(ENGINE_FILE).is_file() && engine.join(ENGINE_RUNTIME).is_file()
}

pub fn asr_ready(app_data: &Path) -> bool {
    discover_asr_files(&asr_dir(app_data)).is_some()
}

pub fn speaker_ready(app_data: &Path) -> bool {
    speaker_path(app_data).is_file()
}

pub fn diarization_model_installed(app_data: &Path) -> bool {
    find_onnx(&diarization_dir(app_data), "model").is_some()
}

/// Offline diarization needs both the pyannote segmentation model and the
/// speaker embedding model.
pub fn diarization_ready(app_data: &Path) -> bool {
    speaker_ready(app_data) && diarization_model_installed(app_data)
}

/// True when live captions can use the local streaming engine.
pub fn streaming_ready(app_data: &Path) -> bool {
    engine_ready(app_data) && asr_ready(app_data)
}

fn directory_bytes(directory: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                directory_bytes(&path)
            } else {
                fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0)
            }
        })
        .sum()
}

pub fn status(app_data: &Path) -> SherpaStatus {
    let mut total_bytes = 0;
    let engine_bytes = directory_bytes(&engine_dir(app_data));
    let asr_model_bytes = directory_bytes(&asr_dir(app_data));
    let speaker_model_bytes = fs::metadata(speaker_path(app_data))
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let diarization_model_bytes = directory_bytes(&diarization_dir(app_data));
    if engine_ready(app_data) {
        total_bytes += engine_bytes;
    }
    if asr_ready(app_data) {
        total_bytes += asr_model_bytes;
    }
    if speaker_ready(app_data) {
        total_bytes += speaker_model_bytes;
    }
    if diarization_model_installed(app_data) {
        total_bytes += diarization_model_bytes;
    }
    SherpaStatus {
        engine_installed: engine_ready(app_data),
        engine_bytes,
        engine_version: ENGINE_VERSION,
        asr_model_installed: asr_ready(app_data),
        asr_model: ASR_ID,
        asr_model_label: ASR_LABEL,
        asr_model_bytes,
        speaker_model_installed: speaker_ready(app_data),
        speaker_model_bytes,
        diarization_installed: diarization_model_installed(app_data),
        diarization_model_bytes,
        total_bytes,
        ready: streaming_ready(app_data),
    }
}

#[tauri::command]
pub fn get_sherpa_status(app: AppHandle) -> Result<SherpaStatus, String> {
    let app_data = app_data(&app)?;
    Ok(status(&app_data))
}

#[tauri::command]
pub async fn download_sherpa(
    kind: String,
    app: AppHandle,
    state: tauri::State<'_, SherpaDownloadState>,
) -> Result<SherpaStatus, String> {
    let _guard = begin_download(&state, &kind)?;
    let app_data = app_data(&app)?;
    match kind.as_str() {
        "engine" => install_engine(&app, &app_data).await?,
        "asr" => install_asr_model(&app, &app_data).await?,
        "speaker" => install_speaker_model(&app, &app_data).await?,
        "diarization" => install_diarization_model(&app, &app_data).await?,
        other => return Err(format!("未知的本地引擎类型：{other}")),
    }
    Ok(status(&app_data))
}

fn begin_download<'a>(
    state: &'a SherpaDownloadState,
    key: &str,
) -> Result<DownloadGuard<'a>, String> {
    let mut slot = state.0.lock().expect("sherpa download state poisoned");
    if let Some(active) = slot.as_ref() {
        return Err(format!("已有本地语音引擎下载任务：{active}"));
    }
    *slot = Some(key.to_owned());
    Ok(DownloadGuard(&state.0))
}

struct DownloadGuard<'a>(&'a Mutex<Option<String>>);

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().expect("sherpa download state poisoned") = None;
    }
}

fn app_data(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))
}

async fn install_engine(app: &AppHandle, app_data: &Path) -> Result<(), String> {
    if engine_ready(app_data) {
        return Ok(());
    }
    let archive = root(app_data).join("sherpa-onnx-win-x64.tar.bz2");
    fs::create_dir_all(root(app_data)).map_err(|error| error.to_string())?;
    download(
        app,
        ENGINE_URL,
        &archive,
        ENGINE_SIZE,
        Some(ENGINE_SHA),
        "engine",
    )
    .await?;
    let staging = root(app_data).join("engine-staging");
    let _ = fs::remove_dir_all(&staging);
    extract_archive(&archive, &staging)?;
    find_file(&staging, ENGINE_FILE)
        .ok_or_else(|| "解压后未找到 sherpa-onnx C 接口库".to_owned())?;
    let target = engine_dir(app_data);
    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target).map_err(|error| error.to_string())?;
    // Flatten every DLL (onnxruntime, the C API, and any runtime dependency)
    // next to each other so LoadLibraryExW resolves them from one directory.
    copy_dlls(&staging, &target)?;
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_file(&archive);
    if !engine_ready(app_data) {
        return Err("本地引擎解压不完整".into());
    }
    Ok(())
}

async fn install_asr_model(app: &AppHandle, app_data: &Path) -> Result<(), String> {
    if asr_ready(app_data) {
        return Ok(());
    }
    let archive = models_dir(app_data).join("zh-14m.tar.bz2");
    fs::create_dir_all(models_dir(app_data)).map_err(|error| error.to_string())?;
    download(app, ASR_URL, &archive, ASR_SIZE, None, "asr").await?;
    let staging = models_dir(app_data).join("asr-staging");
    let _ = fs::remove_dir_all(&staging);
    extract_archive(&archive, &staging)?;
    let tokens =
        find_file(&staging, "tokens.txt").ok_or_else(|| "流式模型缺少 tokens.txt".to_owned())?;
    let source = tokens
        .parent()
        .ok_or_else(|| "流式模型目录无效".to_owned())?
        .to_owned();
    let target = asr_dir(app_data);
    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(&source)
        .map_err(|error| error.to_string())?
        .flatten()
    {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().unwrap_or_default();
            fs::copy(&path, target.join(name)).map_err(|error| error.to_string())?;
        }
    }
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_file(&archive);
    if !asr_ready(app_data) {
        return Err("流式模型缺少 encoder/decoder/joiner".into());
    }
    Ok(())
}

async fn install_speaker_model(app: &AppHandle, app_data: &Path) -> Result<(), String> {
    if speaker_ready(app_data) {
        return Ok(());
    }
    fs::create_dir_all(models_dir(app_data)).map_err(|error| error.to_string())?;
    download(
        app,
        SPEAKER_URL,
        &speaker_path(app_data),
        SPEAKER_SIZE,
        None,
        "speaker",
    )
    .await
}

async fn install_diarization_model(app: &AppHandle, app_data: &Path) -> Result<(), String> {
    if diarization_model_installed(app_data) {
        return Ok(());
    }
    fs::create_dir_all(models_dir(app_data)).map_err(|error| error.to_string())?;
    let archive = models_dir(app_data).join("pyannote-3.0.tar.bz2");
    download(
        app,
        DIARIZATION_URL,
        &archive,
        DIARIZATION_SIZE,
        None,
        "diarization",
    )
    .await?;
    let staging = models_dir(app_data).join("pyannote-staging");
    let _ = fs::remove_dir_all(&staging);
    extract_archive(&archive, &staging)?;
    let model = find_onnx(&staging, "model").ok_or_else(|| "分离模型缺少 model.onnx".to_owned())?;
    let source = model
        .parent()
        .ok_or_else(|| "分离模型目录无效".to_owned())?
        .to_owned();
    let target = diarization_dir(app_data);
    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(&source)
        .map_err(|error| error.to_string())?
        .flatten()
    {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "onnx")
        {
            let name = path.file_name().unwrap_or_default();
            fs::copy(&path, target.join(name)).map_err(|error| error.to_string())?;
        }
    }
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_file(&archive);
    if !diarization_model_installed(app_data) {
        return Err("分离模型安装不完整".into());
    }
    Ok(())
}

async fn download(
    app: &AppHandle,
    url: &str,
    destination: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
    kind: &'static str,
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
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|error| format!("下载中断：{error}"))?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|error| format!("写入下载文件失败：{error}"))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded.saturating_sub(reported) >= 512 * 1024 || downloaded == total {
            reported = downloaded;
            let _ = app.emit(
                "sherpa-download",
                DownloadProgress {
                    kind,
                    downloaded,
                    total,
                },
            );
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|error| format!("写入下载文件失败：{error}"))?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    let valid_sha = expected_sha256.is_none_or(|expected| actual.eq_ignore_ascii_case(expected));
    if downloaded != expected_size || !valid_sha {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!(
            "下载文件校验失败：期望 {expected_size} 字节，实际 {downloaded} 字节（sha256 {actual}）"
        ));
    }
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|error| format!("无法保存下载文件：{error}"))?;
    let _ = app.emit(
        "sherpa-download",
        DownloadProgress {
            kind,
            downloaded: expected_size,
            total: expected_size,
        },
    );
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let file = File::open(archive).map_err(|error| format!("无法打开压缩包：{error}"))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(destination)
        .map_err(|error| format!("解压失败：{error}"))?;
    Ok(())
}

fn copy_dlls(source: &Path, target: &Path) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(source) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_dlls(&path, target)?;
        } else if path.extension().is_some_and(|extension| extension == "dll") {
            let name = path.file_name().unwrap_or_default();
            fs::copy(&path, target.join(name)).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn find_file(directory: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

fn find_onnx(directory: &Path, part: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_onnx(directory, part, &mut candidates);
    candidates.sort_by_key(|path| {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // Prefer the quantized weights; they are ~4x faster and the DIAR-1
        // measurements showed no quality loss for the segmentation model.
        (!name.contains("int8"), name)
    });
    candidates.into_iter().next()
}

fn collect_onnx(directory: &Path, part: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_onnx(&path, part, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "onnx")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(part))
        {
            out.push(path);
        }
    }
}

struct AsrFiles {
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    tokens: PathBuf,
}

fn discover_asr_files(directory: &Path) -> Option<AsrFiles> {
    Some(AsrFiles {
        encoder: find_onnx(directory, "encoder")?,
        decoder: find_onnx(directory, "decoder")?,
        joiner: find_onnx(directory, "joiner")?,
        tokens: find_file(directory, "tokens.txt")?,
    })
}

// ---------------------------------------------------------------------------
// C API
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ParaformerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SingleModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OnlineModelConfig {
    transducer: TransducerModelConfig,
    paraformer: ParaformerModelConfig,
    zipformer2_ctc: SingleModelConfig,
    tokens: *const c_char,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    tokens_buf: *const c_char,
    tokens_buf_size: i32,
    nemo_ctc: SingleModelConfig,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OnlineCtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OnlineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OnlineModelConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    enable_endpoint: i32,
    rule1_min_trailing_silence: f32,
    rule2_min_trailing_silence: f32,
    rule3_min_utterance_length: f32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    ctc_fst_decoder_config: OnlineCtcFstDecoderConfig,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hotwords_buf: *const c_char,
    hotwords_buf_size: i32,
    hr: HomophoneReplacerConfig,
}

#[repr(C)]
struct OnlineRecognizerResult {
    text: *const c_char,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    timestamps: *mut f32,
    count: i32,
    json: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SpeakerEmbeddingExtractorConfig {
    model: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PyannoteModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SegmentationModelConfig {
    pyannote: PyannoteModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FastClusteringConfig {
    num_clusters: i32,
    threshold: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DiarizationConfig {
    segmentation: SegmentationModelConfig,
    embedding: SpeakerEmbeddingExtractorConfig,
    clustering: FastClusteringConfig,
    min_duration_on: f32,
    min_duration_off: f32,
}

#[repr(C)]
struct DiarizationSegment {
    start: f32,
    end: f32,
    speaker: i32,
}

#[allow(dead_code)]
enum OnlineRecognizer {}
#[allow(dead_code)]
enum OnlineStream {}
#[allow(dead_code)]
enum SpeakerEmbeddingExtractor {}
#[allow(dead_code)]
enum OfflineDiarization {}
#[allow(dead_code)]
enum DiarizationResult {}

type CreateRecognizer =
    unsafe extern "C" fn(*const OnlineRecognizerConfig) -> *const OnlineRecognizer;
type DestroyRecognizer = unsafe extern "C" fn(*const OnlineRecognizer);
type CreateStream = unsafe extern "C" fn(*const OnlineRecognizer) -> *const OnlineStream;
type DestroyStream = unsafe extern "C" fn(*const OnlineStream);
type AcceptWaveform = unsafe extern "C" fn(*const OnlineStream, i32, *const f32, i32);
type IsReady = unsafe extern "C" fn(*const OnlineRecognizer, *const OnlineStream) -> i32;
type Decode = unsafe extern "C" fn(*const OnlineRecognizer, *const OnlineStream);
type GetResult = unsafe extern "C" fn(
    *const OnlineRecognizer,
    *const OnlineStream,
) -> *const OnlineRecognizerResult;
type DestroyResult = unsafe extern "C" fn(*const OnlineRecognizerResult);
type ResetStream = unsafe extern "C" fn(*const OnlineRecognizer, *const OnlineStream);
type IsEndpoint = unsafe extern "C" fn(*const OnlineRecognizer, *const OnlineStream) -> i32;

type CreateExtractor = unsafe extern "C" fn(
    *const SpeakerEmbeddingExtractorConfig,
) -> *const SpeakerEmbeddingExtractor;
type DestroyExtractor = unsafe extern "C" fn(*const SpeakerEmbeddingExtractor);
type ExtractorDim = unsafe extern "C" fn(*const SpeakerEmbeddingExtractor) -> i32;
type ExtractorCreateStream =
    unsafe extern "C" fn(*const SpeakerEmbeddingExtractor) -> *const OnlineStream;
type ExtractorReady =
    unsafe extern "C" fn(*const SpeakerEmbeddingExtractor, *const OnlineStream) -> i32;
type ComputeEmbedding =
    unsafe extern "C" fn(*const SpeakerEmbeddingExtractor, *const OnlineStream) -> *const f32;
type DestroyEmbedding = unsafe extern "C" fn(*const f32);

type CreateDiarization =
    unsafe extern "C" fn(*const DiarizationConfig) -> *const OfflineDiarization;
type DestroyDiarization = unsafe extern "C" fn(*const OfflineDiarization);
type DiarizationSampleRate = unsafe extern "C" fn(*const OfflineDiarization) -> i32;
type DiarizationProcess =
    unsafe extern "C" fn(*const OfflineDiarization, *const f32, i32) -> *const DiarizationResult;
type DiarizationNumSegments = unsafe extern "C" fn(*const DiarizationResult) -> i32;
type DiarizationSort = unsafe extern "C" fn(*const DiarizationResult) -> *const DiarizationSegment;
type DiarizationDestroySegment = unsafe extern "C" fn(*const DiarizationSegment);
type DiarizationDestroyResult = unsafe extern "C" fn(*const DiarizationResult);

#[cfg(windows)]
struct Dll(*mut c_void);

#[cfg(windows)]
unsafe impl Send for Dll {}
#[cfg(windows)]
unsafe impl Sync for Dll {}

#[cfg(windows)]
impl Dll {
    fn load(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // LOAD_WITH_ALTERED_SEARCH_PATH makes Windows resolve onnxruntime.dll
        // from the engine directory before PATH.
        let handle = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0x0000_0008) };
        if handle.is_null() {
            return Err(format!(
                "无法加载 {}：{}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(handle))
    }

    unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Result<T, String> {
        let address = unsafe { GetProcAddress(self.0, name.as_ptr()) };
        if address.is_null() {
            return Err(format!(
                "本地语音引擎缺少符号 {}",
                String::from_utf8_lossy(name)
            ));
        }
        Ok(unsafe { std::mem::transmute_copy(&address) })
    }
}

#[cfg(windows)]
impl Drop for Dll {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

pub struct SherpaApi {
    #[cfg(windows)]
    _dll: Dll,
    create_recognizer: CreateRecognizer,
    destroy_recognizer: DestroyRecognizer,
    create_stream: CreateStream,
    destroy_stream: DestroyStream,
    accept_waveform: AcceptWaveform,
    is_ready: IsReady,
    decode: Decode,
    get_result: GetResult,
    destroy_result: DestroyResult,
    reset_stream: ResetStream,
    is_endpoint: IsEndpoint,
    create_extractor: CreateExtractor,
    destroy_extractor: DestroyExtractor,
    extractor_dim: ExtractorDim,
    extractor_create_stream: ExtractorCreateStream,
    extractor_ready: ExtractorReady,
    compute_embedding: ComputeEmbedding,
    destroy_embedding: DestroyEmbedding,
    create_diarization: CreateDiarization,
    destroy_diarization: DestroyDiarization,
    diarization_sample_rate: DiarizationSampleRate,
    diarization_process: DiarizationProcess,
    diarization_num_segments: DiarizationNumSegments,
    diarization_sort: DiarizationSort,
    diarization_destroy_segment: DiarizationDestroySegment,
    diarization_destroy_result: DiarizationDestroyResult,
}

impl SherpaApi {
    #[cfg(windows)]
    fn load(app_data: &Path) -> Result<Self, String> {
        let dll = Dll::load(&engine_dir(app_data).join(ENGINE_FILE))?;
        unsafe {
            Ok(Self {
                create_recognizer: dll.symbol(b"SherpaOnnxCreateOnlineRecognizer\0")?,
                destroy_recognizer: dll.symbol(b"SherpaOnnxDestroyOnlineRecognizer\0")?,
                create_stream: dll.symbol(b"SherpaOnnxCreateOnlineStream\0")?,
                destroy_stream: dll.symbol(b"SherpaOnnxDestroyOnlineStream\0")?,
                accept_waveform: dll.symbol(b"SherpaOnnxOnlineStreamAcceptWaveform\0")?,
                is_ready: dll.symbol(b"SherpaOnnxIsOnlineStreamReady\0")?,
                decode: dll.symbol(b"SherpaOnnxDecodeOnlineStream\0")?,
                get_result: dll.symbol(b"SherpaOnnxGetOnlineStreamResult\0")?,
                destroy_result: dll.symbol(b"SherpaOnnxDestroyOnlineRecognizerResult\0")?,
                reset_stream: dll.symbol(b"SherpaOnnxOnlineStreamReset\0")?,
                is_endpoint: dll.symbol(b"SherpaOnnxOnlineStreamIsEndpoint\0")?,
                create_extractor: dll.symbol(b"SherpaOnnxCreateSpeakerEmbeddingExtractor\0")?,
                destroy_extractor: dll.symbol(b"SherpaOnnxDestroySpeakerEmbeddingExtractor\0")?,
                extractor_dim: dll.symbol(b"SherpaOnnxSpeakerEmbeddingExtractorDim\0")?,
                extractor_create_stream: dll
                    .symbol(b"SherpaOnnxSpeakerEmbeddingExtractorCreateStream\0")?,
                extractor_ready: dll.symbol(b"SherpaOnnxSpeakerEmbeddingExtractorIsReady\0")?,
                compute_embedding: dll
                    .symbol(b"SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding\0")?,
                destroy_embedding: dll
                    .symbol(b"SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding\0")?,
                create_diarization: dll.symbol(b"SherpaOnnxCreateOfflineSpeakerDiarization\0")?,
                destroy_diarization: dll.symbol(b"SherpaOnnxDestroyOfflineSpeakerDiarization\0")?,
                diarization_sample_rate: dll
                    .symbol(b"SherpaOnnxOfflineSpeakerDiarizationGetSampleRate\0")?,
                diarization_process: dll.symbol(b"SherpaOnnxOfflineSpeakerDiarizationProcess\0")?,
                diarization_num_segments: dll
                    .symbol(b"SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments\0")?,
                diarization_sort: dll
                    .symbol(b"SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime\0")?,
                diarization_destroy_segment: dll
                    .symbol(b"SherpaOnnxOfflineSpeakerDiarizationDestroySegment\0")?,
                diarization_destroy_result: dll
                    .symbol(b"SherpaOnnxOfflineSpeakerDiarizationDestroyResult\0")?,
                _dll: dll,
            })
        }
    }

    #[cfg(not(windows))]
    fn load(_app_data: &Path) -> Result<Self, String> {
        Err("本地流式字幕目前只支持 Windows".into())
    }
}

static API: Mutex<Option<(PathBuf, Arc<SherpaApi>)>> = Mutex::new(None);

fn api(app_data: &Path) -> Result<Arc<SherpaApi>, String> {
    let mut slot = API.lock().expect("sherpa api state poisoned");
    if let Some((path, api)) = slot.as_ref()
        && path == app_data
    {
        return Ok(Arc::clone(api));
    }
    let loaded = Arc::new(SherpaApi::load(app_data)?);
    *slot = Some((app_data.to_owned(), Arc::clone(&loaded)));
    Ok(loaded)
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("路径包含非法字符：{}", path.display()))
}

fn required_c_path(path: &Path) -> Result<CString, String> {
    if !path.is_file() {
        return Err(format!("缺少模型文件：{}", path.display()));
    }
    c_path(path)
}

/// One finalized utterance produced by the streaming recognizer.
pub struct FinalChunk {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// 16 kHz mono audio for the utterance, used for speaker embeddings.
    pub audio: Vec<f32>,
}

pub struct StreamingAsr {
    api: Arc<SherpaApi>,
    recognizer: *const OnlineRecognizer,
    stream: *const OnlineStream,
    fed_samples: u64,
    segment_start_samples: u64,
    segment_audio: Vec<f32>,
    partial: String,
}

unsafe impl Send for StreamingAsr {}

impl Drop for StreamingAsr {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy_stream)(self.stream);
            (self.api.destroy_recognizer)(self.recognizer);
        }
    }
}

impl StreamingAsr {
    pub fn new(app_data: &Path) -> Result<Self, String> {
        let api = api(app_data)?;
        let files = discover_asr_files(&asr_dir(app_data))
            .ok_or_else(|| "本地流式模型不完整，请重新下载".to_owned())?;
        let encoder = required_c_path(&files.encoder)?;
        let decoder = required_c_path(&files.decoder)?;
        let joiner = required_c_path(&files.joiner)?;
        let tokens = required_c_path(&files.tokens)?;
        let provider = CString::new("cpu").unwrap();
        let decoding = CString::new("greedy_search").unwrap();

        let mut config = OnlineRecognizerConfig {
            feat_config: FeatureConfig {
                sample_rate: ASR_SAMPLE_RATE,
                feature_dim: 80,
            },
            ..Default::default()
        };
        config.model_config.transducer = TransducerModelConfig {
            encoder: encoder.as_ptr(),
            decoder: decoder.as_ptr(),
            joiner: joiner.as_ptr(),
        };
        config.model_config.tokens = tokens.as_ptr();
        config.model_config.num_threads = 4;
        config.model_config.provider = provider.as_ptr();
        config.model_config.debug = 0;
        config.decoding_method = decoding.as_ptr();
        config.max_active_paths = 4;
        config.enable_endpoint = 1;
        // 0.8 s after speech keeps the caption close to the speaker's pause;
        // the default 1.2 s feels laggy in a live transcript.
        config.rule1_min_trailing_silence = 2.0;
        config.rule2_min_trailing_silence = 0.8;
        config.rule3_min_utterance_length = 20.0;

        let recognizer = unsafe { (api.create_recognizer)(&config) };
        if recognizer.is_null() {
            return Err("无法创建本地流式识别器，请检查模型文件".into());
        }
        let stream = unsafe { (api.create_stream)(recognizer) };
        if stream.is_null() {
            unsafe { (api.destroy_recognizer)(recognizer) };
            return Err("无法创建本地流式识别流".into());
        }
        Ok(Self {
            api,
            recognizer,
            stream,
            fed_samples: 0,
            segment_start_samples: 0,
            segment_audio: Vec::new(),
            partial: String::new(),
        })
    }

    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// Feeds 24 kHz mono PCM16 and returns payload for every completed
    /// utterance. Partial text is updated on every push.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<FinalChunk>, String> {
        if pcm.is_empty() {
            return Ok(Vec::new());
        }
        let samples: Vec<f32> = pcm
            .iter()
            .map(|value| f32::from(*value) / 32_768.0)
            .collect();
        unsafe {
            (self.api.accept_waveform)(
                self.stream,
                SAMPLE_RATE,
                samples.as_ptr(),
                samples.len() as i32,
            );
            while (self.api.is_ready)(self.recognizer, self.stream) != 0 {
                (self.api.decode)(self.recognizer, self.stream);
            }
        }
        self.segment_audio.extend_from_slice(&samples);
        // rule3 caps an utterance at 20 s, so this only guards against a
        // misconfigured model that never reports an endpoint.
        if self.segment_audio.len() > SAMPLE_RATE as usize * 30 {
            self.segment_audio
                .drain(..self.segment_audio.len() - SAMPLE_RATE as usize * 30);
        }
        self.fed_samples += samples.len() as u64;

        let (text, endpoint) = self.read_result();
        self.partial = text.trim().to_owned();
        if !endpoint {
            return Ok(Vec::new());
        }

        let start_ms = self.segment_start_samples * 1_000 / SAMPLE_RATE as u64;
        let end_ms = self.fed_samples * 1_000 / SAMPLE_RATE as u64;
        let audio = if text.trim().is_empty() {
            Vec::new()
        } else {
            resample_24k_to_16k(&self.segment_audio)
        };
        let chunk = FinalChunk {
            start_ms,
            end_ms,
            text: text.trim().to_owned(),
            audio,
        };
        unsafe {
            (self.api.reset_stream)(self.recognizer, self.stream);
        }
        self.segment_start_samples = self.fed_samples;
        self.segment_audio.clear();
        self.partial.clear();
        Ok(if chunk.text.is_empty() {
            Vec::new()
        } else {
            vec![chunk]
        })
    }

    fn read_result(&self) -> (String, bool) {
        unsafe {
            let result = (self.api.get_result)(self.recognizer, self.stream);
            if result.is_null() {
                return (String::new(), false);
            }
            let text = if (*result).text.is_null() {
                String::new()
            } else {
                CStr::from_ptr((*result).text)
                    .to_string_lossy()
                    .into_owned()
            };
            (self.api.destroy_result)(result);
            let endpoint = (self.api.is_endpoint)(self.recognizer, self.stream) != 0;
            (text, endpoint)
        }
    }
}

/// Linear 24 kHz → 16 kHz conversion for the speaker embedding model.
pub fn resample_24k_to_16k(input: &[f32]) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    let ratio = SAMPLE_RATE as f64 / ASR_SAMPLE_RATE as f64;
    let output_len = (input.len() as f64 / ratio).floor() as usize;
    let mut output = Vec::with_capacity(output_len);
    for index in 0..output_len {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let first = input.get(left).copied().unwrap_or(0.0);
        let second = input.get(left + 1).copied().unwrap_or(first);
        output.push(first + (second - first) * fraction);
    }
    output
}

pub struct SpeakerEngine {
    api: Arc<SherpaApi>,
    extractor: *const SpeakerEmbeddingExtractor,
    dim: usize,
}

unsafe impl Send for SpeakerEngine {}

impl Drop for SpeakerEngine {
    fn drop(&mut self) {
        unsafe { (self.api.destroy_extractor)(self.extractor) };
    }
}

impl SpeakerEngine {
    pub fn new(app_data: &Path) -> Result<Self, String> {
        if !speaker_ready(app_data) {
            return Err("未下载声纹模型".into());
        }
        let api = api(app_data)?;
        let model = required_c_path(&speaker_path(app_data))?;
        let provider = CString::new("cpu").unwrap();
        let config = SpeakerEmbeddingExtractorConfig {
            model: model.as_ptr(),
            num_threads: 2,
            debug: 0,
            provider: provider.as_ptr(),
        };
        let extractor = unsafe { (api.create_extractor)(&config) };
        if extractor.is_null() {
            return Err("无法创建声纹提取器".into());
        }
        let dim = unsafe { (api.extractor_dim)(extractor) };
        Ok(Self {
            api,
            extractor,
            dim: dim.max(0) as usize,
        })
    }

    pub fn compute(&self, samples: &[f32]) -> Result<Vec<f32>, String> {
        if samples.len() < ASR_SAMPLE_RATE as usize / 2 || self.dim == 0 {
            return Err("说话的片段太短，无法判断说话人".into());
        }
        let stream = unsafe { (self.api.extractor_create_stream)(self.extractor) };
        if stream.is_null() {
            return Err("无法创建声纹音频流".into());
        }
        let result = (|| {
            let cap = samples.len().min(ASR_SAMPLE_RATE as usize * 20);
            unsafe {
                (self.api.accept_waveform)(stream, ASR_SAMPLE_RATE, samples.as_ptr(), cap as i32);
            }
            if unsafe { (self.api.extractor_ready)(self.extractor, stream) } == 0 {
                return Err("说话的片段太短，无法判断说话人".into());
            }
            let embedding = unsafe { (self.api.compute_embedding)(self.extractor, stream) };
            if embedding.is_null() {
                return Err("声纹提取失败".into());
            }
            let values = unsafe { std::slice::from_raw_parts(embedding, self.dim).to_vec() };
            unsafe { (self.api.destroy_embedding)(embedding) };
            Ok(values)
        })();
        unsafe { (self.api.destroy_stream)(stream) };
        result
    }
}

/// Online speaker clustering: each embedding joins the most similar centroid
/// above [`SPEAKER_THRESHOLD`], otherwise it starts a new speaker.
#[derive(Default)]
pub struct SpeakerGallery {
    centroids: Vec<Vec<f32>>,
    counts: Vec<u32>,
}

impl SpeakerGallery {
    /// Returns the 0-based speaker index.
    pub fn assign(&mut self, embedding: &[f32]) -> usize {
        let normalized = normalize(embedding);
        if normalized.is_empty() {
            return 0;
        }
        let mut best = None;
        let mut best_score = f32::MIN;
        for (index, centroid) in self.centroids.iter().enumerate() {
            let score = dot(&normalized, centroid);
            if score > best_score {
                best_score = score;
                best = Some(index);
            }
        }
        let index = match best {
            Some(index)
                if best_score >= SPEAKER_THRESHOLD || self.centroids.len() >= MAX_SPEAKERS =>
            {
                index
            }
            _ => {
                self.centroids.push(normalized.clone());
                self.counts.push(0);
                self.centroids.len() - 1
            }
        };
        let count = self.counts[index] as f32;
        let centroid = &mut self.centroids[index];
        for (value, update) in centroid.iter_mut().zip(normalized.iter()) {
            *value = (*value * count + update) / (count + 1.0);
        }
        *centroid = normalize(centroid);
        self.counts[index] += 1;
        index
    }

    #[cfg(test)]
    pub fn speakers(&self) -> usize {
        self.centroids.len()
    }
}

fn normalize(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return Vec::new();
    }
    vector.iter().map(|value| value / norm).collect()
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right.iter()).map(|(a, b)| a * b).sum()
}

/// One speaker turn from offline diarization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiarizedSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: usize,
}

/// Runs pyannote segmentation + voice-print clustering over a whole session.
///
/// The caller supplies 16 kHz mono audio; `min_duration_on`/`off` and the
/// 0.7 threshold follow the DIAR-1 spike measurements.
pub fn diarize(app_data: &Path, samples: &[f32]) -> Result<Vec<DiarizedSpan>, String> {
    if !diarization_ready(app_data) {
        return Err("未下载说话人分离模型".into());
    }
    if samples.len() < ASR_SAMPLE_RATE as usize * 2 {
        return Err("录音太短，无法分离说话人".into());
    }
    // 16 kHz f32 is 64 KB/s; two hours is 460 MB and already stressful for
    // the engine, so longer sessions keep the live labels instead.
    if samples.len() > ASR_SAMPLE_RATE as usize * 7_200 {
        return Err("录音超过 2 小时，暂不做离线说话人分离".into());
    }
    let api = api(app_data)?;
    let segmentation =
        find_onnx(&diarization_dir(app_data), "model").ok_or_else(|| "分离模型缺失".to_owned())?;
    let segmentation = required_c_path(&segmentation)?;
    let embedding = required_c_path(&speaker_path(app_data))?;
    let provider = CString::new("cpu").unwrap();
    let config = DiarizationConfig {
        segmentation: SegmentationModelConfig {
            pyannote: PyannoteModelConfig {
                model: segmentation.as_ptr(),
            },
            num_threads: 4,
            debug: 0,
            provider: provider.as_ptr(),
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: embedding.as_ptr(),
            num_threads: 2,
            debug: 0,
            provider: provider.as_ptr(),
        },
        clustering: FastClusteringConfig {
            num_clusters: 0,
            threshold: 0.7,
        },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };
    let handle = unsafe { (api.create_diarization)(&config) };
    if handle.is_null() {
        return Err("无法创建说话人分离器".into());
    }
    let result = (|| {
        let rate = unsafe { (api.diarization_sample_rate)(handle) };
        if rate != ASR_SAMPLE_RATE {
            return Err(format!("分离器期望 {rate} Hz 音频"));
        }
        let length = samples.len().min(i32::MAX as usize) as i32;
        let processed = unsafe { (api.diarization_process)(handle, samples.as_ptr(), length) };
        if processed.is_null() {
            return Err("说话人分离失败".into());
        }
        let count = unsafe { (api.diarization_num_segments)(processed) }.max(0);
        let mut spans = Vec::new();
        if count > 0 {
            let first = unsafe { (api.diarization_sort)(processed) };
            if !first.is_null() {
                for index in 0..count as usize {
                    let segment = unsafe { &*first.add(index) };
                    if segment.end > segment.start {
                        spans.push(DiarizedSpan {
                            start_ms: (segment.start * 1_000.0).max(0.0) as u64,
                            end_ms: (segment.end * 1_000.0).max(0.0) as u64,
                            speaker: segment.speaker.max(0) as usize,
                        });
                    }
                }
                unsafe { (api.diarization_destroy_segment)(first) };
            }
        }
        unsafe { (api.diarization_destroy_result)(processed) };
        if spans.is_empty() {
            return Err("没有检测到可分离的语音".into());
        }
        Ok(spans)
    })();
    unsafe { (api.destroy_diarization)(handle) };
    result
}

/// Assigns offline speaker labels by segment midpoint. Returns how many
/// segments changed.
pub fn assign_speakers(transcript: &mut crate::asr::Transcript, spans: &[DiarizedSpan]) -> usize {
    let mut assigned = 0;
    for segment in &mut transcript.segments {
        let middle = (segment.start_ms + segment.end_ms) / 2;
        if let Some(span) = spans
            .iter()
            .find(|span| middle >= span.start_ms && middle < span.end_ms)
        {
            segment.speaker = Some(format!("S{}", span.speaker + 1));
            assigned += 1;
        }
    }
    assigned
}

fn decode_chunk(chunk: &crate::asr::AudioChunkRef) -> Result<Vec<f32>, String> {
    let mut samples =
        magnetic_tape_audio::decode_ogg_opus(&chunk.path).map_err(|error| error.to_string())?;
    samples.truncate(chunk.frames as usize);
    Ok(samples)
}

/// Decodes and mixes every committed chunk into 16 kHz mono f32, preserving
/// the session timeline (gaps become silence).
pub fn session_pcm(chunks: &[crate::asr::AudioChunkRef]) -> Result<Vec<f32>, String> {
    use magnetic_tape_audio::Track;
    use std::collections::BTreeMap;

    let mut slots: BTreeMap<u64, [Option<&crate::asr::AudioChunkRef>; 2]> = BTreeMap::new();
    for chunk in chunks {
        let slot = slots.entry(chunk.start_frame).or_default();
        match chunk.track {
            Track::Microphone => slot[0] = Some(chunk),
            Track::System => slot[1] = Some(chunk),
        }
    }
    let mut output = Vec::new();
    let mut cursor = 0_u64;
    for (start, pair) in slots {
        if start > cursor {
            output.resize(output.len() + ((start - cursor) / 3) as usize, 0.0);
        }
        let microphone = pair[0].map(decode_chunk).transpose()?;
        let system = pair[1].map(decode_chunk).transpose()?;
        let length = microphone
            .as_ref()
            .map(Vec::len)
            .unwrap_or(0)
            .max(system.as_ref().map(Vec::len).unwrap_or(0));
        let mut index = 0;
        while index < length {
            let mut sum = 0.0;
            let mut count = 0;
            for offset in 0..3 {
                let position = index + offset;
                if position >= length {
                    break;
                }
                let left = microphone
                    .as_ref()
                    .and_then(|samples| samples.get(position))
                    .copied()
                    .unwrap_or(0.0);
                let right = system
                    .as_ref()
                    .and_then(|samples| samples.get(position))
                    .copied()
                    .unwrap_or(0.0);
                sum += (left + right).clamp(-1.0, 1.0);
                count += 1;
            }
            output.push(sum / count.max(1) as f32);
            index += 3;
        }
        cursor = start + length as u64;
    }
    Ok(output)
}

/// Builds the segment the live pipeline stores. `speaker` is `None` when the
/// embedding is missing, so the UI never invents a speaker.
pub fn labeled_segment(chunk: &FinalChunk, speaker: Option<usize>) -> TranscriptSegment {
    TranscriptSegment {
        start_ms: chunk.start_ms,
        end_ms: chunk.end_ms.max(chunk.start_ms + 1),
        speaker: speaker.map(|index| format!("S{}", index + 1)),
        text: chunk.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_24k_to_16k_length() {
        let output = resample_24k_to_16k(&vec![0.0; 24_000]);
        assert_eq!(output.len(), 16_000);
    }

    #[test]
    fn resamples_without_dropping_the_last_sample() {
        let mut input = vec![0.0; 24_000];
        input[23_999] = 1.0;
        let output = resample_24k_to_16k(&input);
        assert_eq!(output.len(), 16_000);
        assert!(output.last().copied().unwrap_or(0.0) >= 0.0);
    }

    #[test]
    fn gallery_keeps_one_speaker_for_similar_embeddings() {
        let mut gallery = SpeakerGallery::default();
        let first = gallery.assign(&[1.0, 0.0, 0.0, 0.0]);
        let second = gallery.assign(&[0.99, 0.1, 0.0, 0.0]);
        assert_eq!(first, 0);
        assert_eq!(second, 0);
        assert_eq!(gallery.speakers(), 1);
    }

    #[test]
    fn gallery_separates_orthogonal_speakers() {
        let mut gallery = SpeakerGallery::default();
        assert_eq!(gallery.assign(&[1.0, 0.0, 0.0, 0.0]), 0);
        assert_eq!(gallery.assign(&[0.0, 1.0, 0.0, 0.0]), 1);
        assert_eq!(gallery.assign(&[0.1, 0.95, 0.0, 0.0]), 1);
        assert_eq!(gallery.speakers(), 2);
    }

    #[test]
    fn gallery_folds_extra_voices_into_the_nearest_speaker() {
        let mut gallery = SpeakerGallery::default();
        for index in 0..MAX_SPEAKERS {
            let mut embedding = vec![0.0; MAX_SPEAKERS];
            embedding[index] = 1.0;
            assert_eq!(gallery.assign(&embedding), index);
        }
        assert_eq!(gallery.speakers(), MAX_SPEAKERS);
        assert_eq!(gallery.assign(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]), 0);
    }

    #[test]
    fn labels_default_to_none_when_the_embedding_is_missing() {
        let chunk = FinalChunk {
            start_ms: 1_000,
            end_ms: 2_000,
            text: "你好".into(),
            audio: Vec::new(),
        };
        assert!(labeled_segment(&chunk, None).speaker.is_none());
        assert_eq!(
            labeled_segment(&chunk, Some(0)).speaker.as_deref(),
            Some("S1")
        );
    }

    #[test]
    fn status_reports_missing_engine_and_models() {
        let directory = tempfile::tempdir().unwrap();
        let status = status(directory.path());
        assert!(!status.engine_installed);
        assert!(!status.asr_model_installed);
        assert!(!status.speaker_model_installed);
        assert!(!status.ready);
        assert_eq!(status.total_bytes, 0);
    }

    #[test]
    fn find_file_walks_nested_directories() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("model/encoder");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("tokens.txt"), b"a").unwrap();
        assert_eq!(
            find_file(directory.path(), "tokens.txt"),
            Some(nested.join("tokens.txt"))
        );
        assert!(find_file(directory.path(), "missing").is_none());
    }

    #[test]
    fn discover_asr_files_prefers_int8_weights() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("encoder.onnx"), b"x").unwrap();
        fs::write(directory.path().join("encoder.int8.onnx"), b"x").unwrap();
        fs::write(directory.path().join("decoder.onnx"), b"x").unwrap();
        fs::write(directory.path().join("joiner.onnx"), b"x").unwrap();
        fs::write(directory.path().join("tokens.txt"), b"a").unwrap();
        let files = discover_asr_files(directory.path()).unwrap();
        assert_eq!(files.encoder.file_name().unwrap(), "encoder.int8.onnx");
    }

    #[test]
    fn download_state_rejects_concurrent_tasks_and_clears_on_drop() {
        let state = SherpaDownloadState::default();
        {
            let _guard = begin_download(&state, "engine").unwrap();
            assert!(begin_download(&state, "asr").is_err());
            assert_eq!(state.0.lock().unwrap().as_deref(), Some("engine"));
        }
        assert!(state.0.lock().unwrap().is_none());
    }

    #[test]
    fn assign_speakers_uses_the_segment_midpoint() {
        let mut transcript = crate::asr::Transcript {
            provider: "sherpa".into(),
            model: "zh-14m".into(),
            language: "zh-CN".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![
                crate::asr::TranscriptSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    speaker: None,
                    text: "甲".into(),
                },
                crate::asr::TranscriptSegment {
                    start_ms: 1_000,
                    end_ms: 4_000,
                    speaker: None,
                    text: "乙".into(),
                },
            ],
            created_at_epoch_ms: 0,
        };
        let spans = vec![
            DiarizedSpan {
                start_ms: 0,
                end_ms: 900,
                speaker: 0,
            },
            DiarizedSpan {
                start_ms: 900,
                end_ms: 4_000,
                speaker: 1,
            },
        ];
        assert_eq!(assign_speakers(&mut transcript, &spans), 2);
        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("S1"));
        assert_eq!(transcript.segments[1].speaker.as_deref(), Some("S2"));
    }

    #[test]
    fn assign_speakers_leaves_uncovered_segments_alone() {
        let mut transcript = crate::asr::Transcript {
            provider: "sherpa".into(),
            model: "zh-14m".into(),
            language: "zh-CN".into(),
            duration_ms: 10_000,
            text: String::new(),
            segments: vec![crate::asr::TranscriptSegment {
                start_ms: 8_000,
                end_ms: 9_000,
                speaker: Some("S9".into()),
                text: "未覆盖".into(),
            }],
            created_at_epoch_ms: 0,
        };
        let spans = vec![DiarizedSpan {
            start_ms: 0,
            end_ms: 2_000,
            speaker: 0,
        }];
        assert_eq!(assign_speakers(&mut transcript, &spans), 0);
        assert_eq!(transcript.segments[0].speaker.as_deref(), Some("S9"));
    }

    #[test]
    fn diarization_is_not_ready_without_models() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!diarization_ready(directory.path()));
    }

    #[test]
    fn resample_is_stable_for_silence() {
        let output = resample_24k_to_16k(&vec![0.0; 4_800]);
        assert!(output.iter().all(|sample| *sample == 0.0));
    }
}
