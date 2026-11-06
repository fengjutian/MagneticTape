//! Local rolling logs, panic crash reports and diagnostics export.
//!
//! Logs stay on the machine under `<app data>/logs`. The diagnostics archive
//! contains only logs and a short system summary: no audio, no API keys and no
//! transcript text.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LOG_FILE: &str = "app.log";
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LOG_FILES: usize = 5;
const MAX_LOG_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct Logger {
    directory: PathBuf,
    file: Mutex<Option<File>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn logs_dir(app_data: &Path) -> PathBuf {
    app_data.join("logs")
}

/// Creates the log directory, prunes old files and installs the panic hook.
pub fn init(app_data: &Path) {
    let directory = logs_dir(app_data);
    if let Err(error) = fs::create_dir_all(&directory) {
        eprintln!("failed to create log directory: {error}");
        return;
    }
    let _ = prune_old(&directory);
    let _ = rotate_if_needed(&directory);
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(LOG_FILE))
    {
        Ok(file) => {
            if LOGGER
                .set(Logger {
                    directory: directory.clone(),
                    file: Mutex::new(Some(file)),
                })
                .is_ok()
            {
                install_panic_hook(directory);
                info("app", "logger initialized");
            }
        }
        Err(error) => eprintln!("failed to open log file: {error}"),
    }
}

pub fn info(target: &str, message: &str) {
    write("INFO", target, message);
}

pub fn warn(target: &str, message: &str) {
    write("WARN", target, message);
}

pub fn error(target: &str, message: &str) {
    write("ERROR", target, message);
}

fn write(level: &str, target: &str, message: &str) {
    let line = format!("[{}] {level:<5} {target}: {message}\n", epoch_ms());
    let Some(logger) = LOGGER.get() else {
        eprintln!("{line}");
        return;
    };
    let Ok(mut guard) = logger.file.lock() else {
        return;
    };
    if guard.is_none() {
        return;
    }
    let path = logger.directory.join(LOG_FILE);
    let oversized = fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES);
    if oversized {
        *guard = None;
        let _ = rotate_files(&logger.directory);
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            *guard = Some(file);
        }
    }
    if let Some(file) = guard.as_mut() {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

fn install_panic_hook(directory: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let report = format!(
            "MagneticTape panic report\n\
             time (epoch ms): {}\n\
             panic: {info}\n\n\
             backtrace:\n{}",
            epoch_ms(),
            std::backtrace::Backtrace::force_capture()
        );
        eprintln!("{report}");
        let path = directory.join(format!("crash-{}.log", epoch_ms()));
        let _ = fs::write(path, report);
    }));
}

fn rotate_if_needed(directory: &Path) -> Result<(), String> {
    let path = directory.join(LOG_FILE);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.len() >= MAX_LOG_BYTES => rotate_files(directory),
        _ => Ok(()),
    }
}

/// Shifts `app.log` to `app.log.1`, keeping at most `MAX_LOG_FILES` files.
fn rotate_files(directory: &Path) -> Result<(), String> {
    let mut paths = Vec::new();
    for index in 1..=MAX_LOG_FILES {
        paths.push(directory.join(format!("{LOG_FILE}.{index}")));
    }
    let _ = fs::remove_file(&paths[MAX_LOG_FILES - 1]);
    for index in (1..MAX_LOG_FILES).rev() {
        let from = &paths[index - 1];
        if from.is_file() {
            let _ = fs::remove_file(&paths[index]);
            fs::rename(from, &paths[index]).map_err(|error| error.to_string())?;
        }
    }
    let current = directory.join(LOG_FILE);
    if current.is_file() {
        let _ = fs::remove_file(&paths[0]);
        fs::rename(&current, &paths[0]).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn prune_old(directory: &Path) -> Result<(), String> {
    let now = SystemTime::now();
    let entries = fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with(LOG_FILE) || name.starts_with("crash-")) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > MAX_LOG_AGE {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Writes a diagnostics zip to `destination` containing the system summary and
/// every log/crash file. Returns the archive path.
pub fn write_diagnostics(
    app_data: &Path,
    destination: &Path,
    summary: &str,
) -> Result<PathBuf, String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = File::create(destination).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    archive
        .start_file("system.txt", options)
        .map_err(|error| error.to_string())?;
    archive
        .write_all(summary.as_bytes())
        .map_err(|error| error.to_string())?;

    let directory = logs_dir(app_data);
    if let Ok(entries) = fs::read_dir(&directory) {
        let mut names = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?.to_owned();
                (path.is_file() && (name.ends_with(".log"))).then_some(name)
            })
            .collect::<Vec<_>>();
        names.sort();
        for name in names {
            let content = match fs::read(directory.join(&name)) {
                Ok(content) => content,
                Err(_) => continue,
            };
            archive
                .start_file(format!("logs/{name}"), options)
                .map_err(|error| error.to_string())?;
            archive
                .write_all(&content)
                .map_err(|error| error.to_string())?;
        }
    }
    archive.finish().map_err(|error| error.to_string())?;
    Ok(destination.to_path_buf())
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
    use std::io::Read;

    #[test]
    fn rotates_and_keeps_bounded_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(LOG_FILE), b"current").unwrap();
        for index in 1..=MAX_LOG_FILES {
            fs::write(
                directory.path().join(format!("{LOG_FILE}.{index}")),
                format!("old-{index}"),
            )
            .unwrap();
        }
        rotate_files(directory.path()).unwrap();
        assert_eq!(
            fs::read(directory.path().join(format!("{LOG_FILE}.1"))).unwrap(),
            b"current"
        );
        assert_eq!(
            fs::read(directory.path().join(format!("{LOG_FILE}.2"))).unwrap(),
            b"old-1"
        );
        let extra: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(LOG_FILE))
            .collect();
        assert_eq!(extra.len(), MAX_LOG_FILES);
    }

    #[test]
    fn diagnostics_contains_summary_and_logs() {
        let directory = tempfile::tempdir().unwrap();
        let logs = logs_dir(directory.path());
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("app.log"), b"hello log").unwrap();
        fs::write(logs.join("crash-1.log"), b"crash report").unwrap();
        let destination = directory.path().join("out/diagnostics.zip");
        let path = write_diagnostics(directory.path(), &destination, "MagneticTape 0.1.0").unwrap();
        assert!(path.is_file());
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut names = Vec::new();
        for index in 0..archive.len() {
            names.push(archive.by_index(index).unwrap().name().to_owned());
        }
        assert!(names.contains(&"system.txt".to_owned()));
        assert!(names.contains(&"logs/app.log".to_owned()));
        assert!(names.contains(&"logs/crash-1.log".to_owned()));
        let mut summary = String::new();
        archive
            .by_name("system.txt")
            .unwrap()
            .read_to_string(&mut summary)
            .unwrap();
        assert!(summary.contains("MagneticTape"));
    }

    #[test]
    fn prune_removes_only_old_log_files() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join(LOG_FILE);
        fs::write(&log, b"x").unwrap();
        let other = directory.path().join("recording.ogg");
        fs::write(&other, b"x").unwrap();
        prune_old(directory.path()).unwrap();
        assert!(log.is_file());
        assert!(other.is_file());
    }
}
