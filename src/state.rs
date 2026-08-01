use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use uuid::Uuid;

use crate::model::BeaconState;

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_environment() -> Result<Self> {
        let root = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .context("HERDR_PLUGIN_STATE_DIR is not set")?;
        Ok(Self::new(PathBuf::from(root)))
    }

    pub fn read(&self) -> Result<BeaconState> {
        self.ensure_root()?;
        let lock = self.open_lock()?;
        lock.lock_shared().context("failed to lock Beacon state")?;
        let result = self.load();
        FileExt::unlock(&lock).context("failed to unlock Beacon state")?;
        result
    }

    pub fn update<T>(&self, mutate: impl FnOnce(&mut BeaconState) -> Result<T>) -> Result<T> {
        self.ensure_root()?;
        let lock = self.open_lock()?;
        lock.lock_exclusive()
            .context("failed to lock Beacon state")?;
        let result = (|| {
            let mut state = self.load()?;
            let value = mutate(&mut state)?;
            state.validate()?;
            self.save(&state)?;
            Ok(value)
        })();
        FileExt::unlock(&lock).context("failed to unlock Beacon state")?;
        result
    }

    fn ensure_root(&self) -> Result<()> {
        if let Ok(metadata) = fs::symlink_metadata(&self.root) {
            if metadata.file_type().is_symlink() {
                bail!("Beacon state directory cannot be a symlink");
            }
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("failed to create {}", self.root.display()))?;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect {}", self.root.display()))
    }

    fn open_lock(&self) -> Result<File> {
        let path = self.root.join("state.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }

    fn load(&self) -> Result<BeaconState> {
        let path = self.root.join("state.json");
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BeaconState::default())
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        let state = serde_json::from_slice::<BeaconState>(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        state.validate()?;
        Ok(state)
    }

    fn save(&self, state: &BeaconState) -> Result<()> {
        let target = self.root.join("state.json");
        let temporary = self.root.join(format!(".state-{}.tmp", Uuid::new_v4()));
        let contents =
            serde_json::to_vec_pretty(state).context("failed to serialize Beacon state")?;
        let result = write_new(&temporary, &contents).and_then(|()| {
            fs::rename(&temporary, &target)
                .with_context(|| format!("failed to replace {}", target.display()))
        });
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn write_new(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::tempdir;

    use super::*;
    use crate::model::{AgentObservation, AgentStatus, ObservationSource};

    fn observation(index: usize) -> AgentObservation {
        AgentObservation {
            pane_id: format!("w1:p{index}"),
            terminal_id: format!("t{index}"),
            workspace_id: "w1".to_string(),
            status: AgentStatus::Done,
            focused: false,
            state_change_seq: index as u64,
        }
    }

    #[test]
    fn round_trip_uses_private_directory_and_files() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("beacon");
        let store = StateStore::new(root.clone());
        store
            .update(|state| {
                state.observe(observation(1), ObservationSource::Event);
                Ok(())
            })
            .unwrap();

        assert_eq!(store.read().unwrap().entries().len(), 1);
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("state.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join("state.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_writers_do_not_lose_entries() {
        let temporary = tempdir().unwrap();
        let store = Arc::new(StateStore::new(temporary.path().join("beacon")));
        let barrier = Arc::new(Barrier::new(17));
        let handles = (1..=16)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .update(|state| {
                            state.observe(observation(index), ObservationSource::Event);
                            Ok(())
                        })
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(store.read().unwrap().entries().len(), 16);
    }

    #[test]
    fn corrupt_state_is_reported_without_overwrite() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("beacon");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("state.json"), "{broken").unwrap();
        let store = StateStore::new(root.clone());

        let error = store.update(|_| Ok(())).unwrap_err();

        assert!(error.to_string().contains("parse"));
        assert_eq!(
            fs::read_to_string(root.join("state.json")).unwrap(),
            "{broken"
        );
    }

    #[test]
    fn unsupported_state_version_is_rejected() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("beacon");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("state.json"),
            r#"{"version":99,"next_ordinal":0,"entries":{},"watermarks":{}}"#,
        )
        .unwrap();
        let store = StateStore::new(root);

        let error = store.read().unwrap_err();

        assert!(error.to_string().contains("version 99"));
    }
}
