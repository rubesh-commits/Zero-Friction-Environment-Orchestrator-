use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use linuxorchestrator_core::paths;
use linuxorchestrator_core::process::{self, RunOptions};
use linuxorchestrator_core::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Local,
    Remote,
}

#[derive(Clone, Debug)]
pub struct Acquisition {
    pub root: PathBuf,
    pub repo_name: String,
    pub display_target: String,
    pub source: SourceKind,
    pub commit: Option<String>,
    pub cache_hit: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AcquireOptions {
    /// Reuse a previously cloned workspace when the URL still matches.
    pub reuse_cache: bool,
    pub clone_timeout: Duration,
}

impl Default for AcquireOptions {
    fn default() -> Self {
        Self {
            reuse_cache: true,
            clone_timeout: Duration::from_secs(900),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CloneMarker {
    url: String,
    commit: Option<String>,
    acquired_at: String,
}

/// Acquire a remote repository into the tool's cache, or return a local path
/// untouched. Remote clones are shallow and staged under
/// `$XDG_DATA_HOME/LinuxOrchestrator/cache/workspaces`.
pub fn acquire(target: &str, options: &AcquireOptions) -> Result<Acquisition> {
    let target = target.trim();
    if target.is_empty() {
        return Err(Error::Message(
            "no repository URL or path was provided".into(),
        ));
    }
    if is_remote(target) {
        acquire_remote(target, options)
    } else {
        acquire_local(target)
    }
}

pub fn is_remote(target: &str) -> bool {
    let lowered = target.to_ascii_lowercase();
    lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("git@")
        || lowered.starts_with("ssh://")
        || lowered.starts_with("git://")
        || lowered.starts_with("file://")
}

fn acquire_local(target: &str) -> Result<Acquisition> {
    let path = PathBuf::from(target);
    if !path.exists() {
        return Err(Error::Message(format!(
            "local path does not exist: {}",
            path.display()
        )));
    }
    let absolute = std::path::absolute(&path).unwrap_or(path);
    let root = if absolute.is_file() {
        absolute
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or(absolute.clone())
    } else {
        absolute.clone()
    };
    let repo_name = paths::repo_name_from_target(&root.display().to_string());
    Ok(Acquisition {
        root,
        repo_name,
        display_target: target.to_string(),
        source: SourceKind::Local,
        commit: None,
        cache_hit: false,
        warnings: Vec::new(),
    })
}

fn acquire_remote(target: &str, options: &AcquireOptions) -> Result<Acquisition> {
    paths::ensure_app_dirs()?;
    let git = process::which("git").ok_or_else(|| {
        Error::Message(
            "Git was not found on PATH. Install Git, or pass a local repository path.".into(),
        )
    })?;
    let repo_name = paths::repo_name_from_target(target);
    let key = format!("{repo_name}-{}", paths::stable_hash(target));
    let final_dir = paths::workspaces_dir().join(&key);
    let marker_path = final_dir.join(".linuxorchestrator.json");

    if options.reuse_cache && final_dir.join(".git").is_dir() {
        if let Ok(marker_text) = std::fs::read_to_string(&marker_path) {
            if let Ok(marker) = serde_json::from_str::<CloneMarker>(&marker_text) {
                if marker.url == target {
                    return Ok(Acquisition {
                        root: final_dir,
                        repo_name,
                        display_target: target.to_string(),
                        source: SourceKind::Remote,
                        commit: marker.commit,
                        cache_hit: true,
                        warnings: vec![
                            "Reused the cached shallow clone. Use `clean --all` to force a fresh clone."
                                .to_string(),
                        ],
                    });
                }
            }
        }
    }

    if final_dir.exists() {
        linuxorchestrator_core::fsutil::remove_tree(&final_dir)?;
    }
    let temp_dir = paths::workspaces_dir().join(format!("{key}.tmp-{}", std::process::id()));
    if temp_dir.exists() {
        linuxorchestrator_core::fsutil::remove_tree(&temp_dir)?;
    }

    println!("Cloning {target} (shallow)...");
    let output = process::run_capture(
        &git.display().to_string(),
        &[
            "clone",
            "--depth",
            "1",
            "--no-tags",
            "--single-branch",
            target,
            &temp_dir.display().to_string(),
        ],
        &RunOptions {
            cwd: Some(paths::workspaces_dir()),
            env: vec![
                ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
                (
                    "GIT_ASKPASS".to_string(),
                    if cfg!(windows) { "echo" } else { "true" }.to_string(),
                ),
            ],
            timeout: options.clone_timeout,
        },
    );
    if !output.success {
        let _ = linuxorchestrator_core::fsutil::remove_tree(&temp_dir);
        let detail = output.combined();
        return Err(Error::Message(format!(
            "git clone failed for {target}: {}",
            if detail.is_empty() {
                output
                    .spawn_error
                    .unwrap_or_else(|| "unknown git error".to_string())
            } else {
                detail
            }
        )));
    }

    let commit = process::run_capture(
        &git.display().to_string(),
        &["-C", &temp_dir.display().to_string(), "rev-parse", "HEAD"],
        &RunOptions::default().timeout(Duration::from_secs(30)),
    )
    .first_line();

    let marker = CloneMarker {
        url: target.to_string(),
        commit: (!commit.is_empty()).then(|| commit.clone()),
        acquired_at: paths::iso8601_now(),
    };
    let marker_text = serde_json::to_string_pretty(&marker)?;
    linuxorchestrator_core::fsutil::write_utf8(
        &temp_dir.join(".linuxorchestrator.json"),
        &marker_text,
    )?;

    std::fs::rename(&temp_dir, &final_dir).map_err(|source| Error::Io {
        context: format!(
            " while moving the clone from {} to {}",
            temp_dir.display(),
            final_dir.display()
        ),
        source,
    })?;

    Ok(Acquisition {
        root: final_dir,
        repo_name,
        display_target: target.to_string(),
        source: SourceKind::Remote,
        commit: (!commit.is_empty()).then_some(commit),
        cache_hit: false,
        warnings: Vec::new(),
    })
}

/// Remove cached clone directories. Workspaces are never the user's original
/// repository, so this is safe; callers still ask for confirmation.
pub fn clean_workspaces() -> Result<Vec<PathBuf>> {
    paths::ensure_app_dirs()?;
    let mut removed = Vec::new();
    for entry in linuxorchestrator_core::fsutil::read_dir_sorted(&paths::workspaces_dir())? {
        if entry.is_dir() {
            linuxorchestrator_core::fsutil::remove_tree(&entry)?;
            removed.push(entry);
        }
    }
    Ok(removed)
}

pub fn workspace_cache_dir() -> PathBuf {
    paths::workspaces_dir()
}

pub fn local_repo_name(path: &Path) -> String {
    paths::repo_name_from_target(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_remote_urls() {
        assert!(is_remote("https://github.com/acme/demo.git"));
        assert!(is_remote("git@github.com:acme/demo.git"));
        assert!(is_remote("ssh://git@example.com/demo"));
        assert!(!is_remote("/work/demo"));
        assert!(!is_remote("./demo"));
    }

    #[test]
    fn local_acquisition_does_not_copy_or_modify() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("linuxorchestrator-acquire-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("main.py"), "print('ok')").unwrap();
        let acquisition = acquire(&dir.display().to_string(), &AcquireOptions::default()).unwrap();
        assert_eq!(acquisition.source, SourceKind::Local);
        assert_eq!(acquisition.root, dir);
        assert!(dir.join("main.py").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
