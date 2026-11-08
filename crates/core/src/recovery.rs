use crate::manifest::{ManifestError, RecoveryManifest};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

const CHECKPOINT_PREFIX: &str = "checkpoint-";
const CHECKPOINT_SUFFIX: &str = ".json";
/// Number of checkpoint files kept per session. Older files are pruned after
/// every successful write so a long meeting cannot grow without bound.
pub const CHECKPOINT_RETENTION: usize = 10;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("recovery I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid recovery manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("cannot encode or decode recovery manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("checkpoint sequence {0} already exists")]
    CheckpointExists(u64),
}

#[derive(Debug, Clone)]
pub struct RecoveryStore {
    directory: PathBuf,
}

impl RecoveryStore {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self, RecoveryError> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    pub fn checkpoint(&self, manifest: &RecoveryManifest) -> Result<PathBuf, RecoveryError> {
        manifest.validate()?;
        let sequence = manifest.checkpoint_sequence;
        let final_path = self.checkpoint_path(sequence);
        if final_path.exists() {
            return Err(RecoveryError::CheckpointExists(sequence));
        }

        let temporary_path = self
            .directory
            .join(format!(".{CHECKPOINT_PREFIX}{sequence:020}.tmp"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;

        let write_result = (|| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, manifest)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::rename(&temporary_path, &final_path)?;
            Ok::<_, RecoveryError>(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result?;
        // Keep only the newest checkpoints. Pruning is best-effort: failing to
        // delete an old file must never fail the checkpoint that was just
        // committed.
        let _ = self.prune(CHECKPOINT_RETENTION);
        Ok(final_path)
    }

    /// Deletes all but the newest `keep` checkpoints and any abandoned
    /// temporary files left by an interrupted write. Returns the number of
    /// files removed.
    pub fn prune(&self, keep: usize) -> Result<usize, RecoveryError> {
        let keep = keep.max(1);
        let mut checkpoints = self.checkpoints()?;
        checkpoints.sort_unstable_by_key(|(sequence, _)| std::cmp::Reverse(*sequence));

        let mut removed = 0;
        for (_, path) in checkpoints.into_iter().skip(keep) {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }

        let temporary_prefix = format!(".{CHECKPOINT_PREFIX}");
        for entry in fs::read_dir(&self.directory)?.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with(&temporary_prefix)
                && name.ends_with(".tmp")
                && fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn load_latest(&self) -> Result<Option<RecoveryManifest>, RecoveryError> {
        let Some((_, path)) = self.checkpoints()?.into_iter().max_by_key(|item| item.0) else {
            return Ok(None);
        };
        let manifest: RecoveryManifest =
            serde_json::from_reader(BufReader::new(File::open(path)?))?;
        manifest.validate()?;
        Ok(Some(manifest))
    }

    fn checkpoints(&self) -> Result<Vec<(u64, PathBuf)>, RecoveryError> {
        let mut checkpoints = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(number) = name
                .strip_prefix(CHECKPOINT_PREFIX)
                .and_then(|name| name.strip_suffix(CHECKPOINT_SUFFIX))
                .and_then(|name| name.parse::<u64>().ok())
            else {
                continue;
            };
            checkpoints.push((number, path));
        }
        Ok(checkpoints)
    }

    fn checkpoint_path(&self, sequence: u64) -> PathBuf {
        self.directory.join(format!(
            "{CHECKPOINT_PREFIX}{sequence:020}{CHECKPOINT_SUFFIX}"
        ))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_highest_valid_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = RecoveryStore::open(directory.path()).unwrap();

        for sequence in [1, 3, 2] {
            let mut manifest = RecoveryManifest::new("session-1");
            manifest.checkpoint_sequence = sequence;
            store.checkpoint(&manifest).unwrap();
        }

        assert_eq!(store.load_latest().unwrap().unwrap().checkpoint_sequence, 3);
    }

    #[test]
    fn never_overwrites_an_existing_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = RecoveryStore::open(directory.path()).unwrap();
        let manifest = RecoveryManifest::new("session-1");
        store.checkpoint(&manifest).unwrap();

        assert!(matches!(
            store.checkpoint(&manifest),
            Err(RecoveryError::CheckpointExists(0))
        ));
    }

    #[test]
    fn prunes_old_checkpoints_and_abandoned_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let store = RecoveryStore::open(directory.path()).unwrap();

        for sequence in 0..15 {
            let mut manifest = RecoveryManifest::new("session-1");
            manifest.checkpoint_sequence = sequence;
            store.checkpoint(&manifest).unwrap();
        }

        assert_eq!(checkpoint_files(directory.path()), CHECKPOINT_RETENTION);
        assert_eq!(
            store.load_latest().unwrap().unwrap().checkpoint_sequence,
            14
        );

        let temporary = directory
            .path()
            .join(format!(".{CHECKPOINT_PREFIX}00000000000000000099.tmp"));
        fs::write(&temporary, b"stale").unwrap();

        store.prune(3).unwrap();

        assert_eq!(checkpoint_files(directory.path()), 3);
        assert!(!temporary.exists());
        assert_eq!(
            store.load_latest().unwrap().unwrap().checkpoint_sequence,
            14
        );
    }

    fn checkpoint_files(directory: &Path) -> usize {
        fs::read_dir(directory)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(CHECKPOINT_PREFIX) && name.ends_with(CHECKPOINT_SUFFIX)
                })
            })
            .count()
    }
}
