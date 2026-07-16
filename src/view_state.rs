//! Durable, versioned popup view preferences.
//!
//! Only the six view booleans are persisted. Reads are deliberately non-destructive:
//! malformed or incompatible documents return a warning and remain on disk.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// On-disk schema version for `view.json`; unsupported versions are ignored in place.
pub const VIEW_STATE_VERSION: u32 = 1;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// The complete and exclusive set of view choices Ilmari remembers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewState {
    pub app: bool,
    pub git: bool,
    pub detail: bool,
    pub time: bool,
    pub output: bool,
    pub stats: bool,
}

/// Result of a non-destructive load: either a valid state or a warning with no mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStateLoad {
    pub state: Option<ViewState>,
    pub warning: Option<String>,
}

impl ViewStateLoad {
    fn missing() -> Self {
        Self { state: None, warning: None }
    }

    fn warning(message: String) -> Self {
        Self { state: None, warning: Some(message) }
    }
}

/// Store located under XDG state home, or disabled when no home can be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewStateStore {
    path: Option<PathBuf>,
}

impl ViewStateStore {
    /// No-op store used when state home cannot be resolved or persistence is unwanted.
    pub fn disabled() -> Self {
        Self { path: None }
    }

    /// Resolve the XDG state path from the given environment map.
    pub fn from_env_map(env: &BTreeMap<String, String>) -> Self {
        Self { path: state_path_from_env(env) }
    }

    #[cfg(test)]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: Some(path.into()) }
    }

    #[cfg(test)]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Read state without modifying a corrupt, unreadable, or incompatible file.
    pub fn load(&self) -> ViewStateLoad {
        let Some(path) = &self.path else {
            return ViewStateLoad::missing();
        };
        let source = match fs::read(path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return ViewStateLoad::missing();
            }
            Err(error) => {
                return ViewStateLoad::warning(format!(
                    "could not read remembered Ilmari views at {}: {error}",
                    path.display()
                ));
            }
        };

        match serde_json::from_slice::<ViewStateDocument>(&source) {
            Ok(document) if document.version == VIEW_STATE_VERSION => {
                ViewStateLoad { state: Some(document.into()), warning: None }
            }
            Ok(document) => ViewStateLoad::warning(format!(
                "ignored remembered Ilmari views at {}: unsupported version {} (expected {})",
                path.display(),
                document.version,
                VIEW_STATE_VERSION
            )),
            Err(error) => ViewStateLoad::warning(format!(
                "ignored corrupt remembered Ilmari views at {}: {error}",
                path.display()
            )),
        }
    }

    /// Atomically replace the state document with a private same-directory file.
    pub fn save(&self, state: ViewState) -> Result<(), ViewStateError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Some(parent) = path.parent() else {
            return Err(ViewStateError::InvalidPath(path.clone()));
        };
        fs::create_dir_all(parent).map_err(|source| ViewStateError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
        let document = ViewStateDocument::from(state);
        let mut bytes = serde_json::to_vec_pretty(&document)
            .expect("serializing a fixed view-state document cannot fail");
        bytes.push(b'\n');
        let (temporary_path, mut file) = create_temporary(parent, path)?;
        let write_result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary_path, path)?;
            sync_directory(parent);
            Ok::<(), io::Error>(())
        })();

        if let Err(source) = write_result {
            let _ = fs::remove_file(&temporary_path);
            return Err(ViewStateError::Replace { path: path.clone(), source });
        }
        Ok(())
    }

    /// Remove remembered state. Clearing an absent or unresolved store succeeds.
    pub fn clear(&self) -> Result<(), ViewStateError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_directory(parent);
                }
                Ok(())
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ViewStateError::Clear { path: path.clone(), source }),
        }
    }
}

/// Resolve `$XDG_STATE_HOME/ilmari/view.json`, then `~/.local/state/ilmari/view.json`.
pub fn state_path_from_env(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    nonempty_path(env.get("XDG_STATE_HOME").map(String::as_str))
        .map(|base| base.join("ilmari").join("view.json"))
        .or_else(|| {
            nonempty_path(env.get("HOME").map(String::as_str))
                .map(|home| home.join(".local").join("state").join("ilmari").join("view.json"))
        })
}

fn nonempty_path(value: Option<&str>) -> Option<PathBuf> {
    value.filter(|value| !value.trim().is_empty()).map(PathBuf::from)
}

/// Create a same-directory private temp file used for atomic view.json replacement.
fn create_temporary(parent: &Path, destination: &Path) -> Result<(PathBuf, File), ViewStateError> {
    let file_name = destination.file_name().and_then(|name| name.to_str()).unwrap_or("view.json");
    for _ in 0..32 {
        let suffix = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary_path =
            parent.join(format!(".{file_name}.tmp-{}-{suffix}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ViewStateError::CreateTemporary { path: temporary_path, source });
            }
        }
    }

    Err(ViewStateError::CreateTemporary {
        path: parent.join(format!(".{file_name}.tmp")),
        source: io::Error::new(io::ErrorKind::AlreadyExists, "temporary file collision limit"),
    })
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

/// Failures creating, replacing, or clearing the remembered view-state file.
#[derive(Debug, Error)]
pub enum ViewStateError {
    #[error("remembered view-state path has no parent directory: {0}")]
    InvalidPath(PathBuf),
    #[error("failed to create remembered view-state directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to create temporary remembered view-state file {path}: {source}")]
    CreateTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to atomically replace remembered view state at {path}: {source}")]
    Replace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to clear remembered view state at {path}: {source}")]
    Clear {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewStateDocument {
    version: u32,
    app: bool,
    git: bool,
    detail: bool,
    time: bool,
    output: bool,
    stats: bool,
}

impl From<ViewState> for ViewStateDocument {
    fn from(state: ViewState) -> Self {
        Self {
            version: VIEW_STATE_VERSION,
            app: state.app,
            git: state.git,
            detail: state.detail,
            time: state.time,
            output: state.output,
            stats: state.stats,
        }
    }
}

impl From<ViewStateDocument> for ViewState {
    fn from(document: ViewStateDocument) -> Self {
        Self {
            app: document.app,
            git: document.git,
            detail: document.detail,
            time: document.time,
            output: document.output,
            stats: document.stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_store(label: &str) -> ViewStateStore {
        let directory = std::env::temp_dir().join(format!(
            "ilmari-view-state-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        ViewStateStore::at(directory.join("ilmari").join("view.json"))
    }

    fn state() -> ViewState {
        ViewState { app: true, git: false, detail: true, time: false, output: true, stats: false }
    }

    #[test]
    fn xdg_path_wins_and_home_is_the_fallback() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/home/tester".to_string());
        assert_eq!(
            state_path_from_env(&env),
            Some(PathBuf::from("/home/tester/.local/state/ilmari/view.json"))
        );

        env.insert("XDG_STATE_HOME".to_string(), "/state".to_string());
        assert_eq!(state_path_from_env(&env), Some(PathBuf::from("/state/ilmari/view.json")));
    }

    #[test]
    fn save_load_replace_and_clear_round_trip_only_views() {
        let store = test_store("round-trip");
        assert_eq!(store.load(), ViewStateLoad::missing());

        store.save(state()).expect("save state");
        assert_eq!(store.load().state, Some(state()));
        let source = fs::read_to_string(store.path().expect("store path")).expect("state source");
        let json: serde_json::Value = serde_json::from_str(&source).expect("valid JSON");
        assert_eq!(json.as_object().expect("object").len(), 7);
        assert_eq!(json["version"], VIEW_STATE_VERSION);
        assert!(json.get("bell").is_none());
        assert!(json.get("selected_pane").is_none());

        let replacement = ViewState { stats: true, ..state() };
        store.save(replacement).expect("replace state");
        assert_eq!(store.load().state, Some(replacement));

        store.clear().expect("clear state");
        store.clear().expect("repeated clear");
        assert!(!store.path().expect("store path").exists());
    }

    #[test]
    fn corrupt_and_incompatible_files_are_warned_and_preserved() {
        let corrupt = test_store("corrupt");
        let corrupt_path = corrupt.path().expect("path");
        fs::create_dir_all(corrupt_path.parent().expect("parent")).expect("directory");
        fs::write(corrupt_path, b"{ definitely not JSON").expect("write corrupt state");
        let loaded = corrupt.load();
        assert!(loaded.state.is_none());
        assert!(loaded.warning.as_deref().is_some_and(|warning| warning.contains("corrupt")));
        assert_eq!(fs::read(corrupt_path).expect("corrupt file remains"), b"{ definitely not JSON");

        let incompatible = test_store("incompatible");
        let incompatible_path = incompatible.path().expect("path");
        fs::create_dir_all(incompatible_path.parent().expect("parent")).expect("directory");
        fs::write(
            incompatible_path,
            br#"{"version":99,"app":true,"git":true,"detail":true,"time":true,"output":true,"stats":true}"#,
        )
        .expect("write incompatible state");
        let loaded = incompatible.load();
        assert!(loaded.state.is_none());
        assert!(loaded.warning.as_deref().is_some_and(|warning| warning.contains("version 99")));
        assert!(incompatible_path.exists());

        let unknown = test_store("unknown-field");
        let unknown_path = unknown.path().expect("path");
        fs::create_dir_all(unknown_path.parent().expect("parent")).expect("directory");
        fs::write(
            unknown_path,
            br#"{"version":1,"app":true,"git":true,"detail":true,"time":true,"output":true,"stats":true,"bell":true}"#,
        )
        .expect("write state with unknown field");
        let loaded = unknown.load();
        assert!(loaded.state.is_none());
        assert!(loaded.warning.as_deref().is_some_and(|warning| warning.contains("unknown field")));
        assert!(unknown_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_state_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let store = test_store("permissions");
        store.save(state()).expect("save state");
        let mode =
            fs::metadata(store.path().expect("path")).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
