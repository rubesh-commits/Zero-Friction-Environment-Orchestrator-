use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use winorchestrator_core::fsutil;
use winorchestrator_core::process::{self, RunOptions};
use winorchestrator_core::{
    Error, HostMatch, MatchStatus, ResolutionPlan, Result, RuntimeKind, Version,
};

mod download;
mod sources;

pub use download::{download_verified, fetch_text_cached, sha256_matches};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub runtime: RuntimeKind,
    pub version: String,
    pub file: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvisionedRuntime {
    pub runtime: RuntimeKind,
    pub version: Version,
    pub home: PathBuf,
    pub executable: PathBuf,
    pub companions: BTreeMap<String, PathBuf>,
    pub from_host: bool,
    pub source: String,
}

impl ProvisionedRuntime {
    /// Directory that must be on PATH for this runtime.
    pub fn path_dir(&self) -> PathBuf {
        match self.runtime {
            RuntimeKind::Java => self.home.join("bin"),
            _ => self.home.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProvisionResult {
    pub runtimes: Vec<ProvisionedRuntime>,
    pub downloads: Vec<DownloadRecord>,
    pub estimated_download_bytes: u64,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProvisionOptions {
    pub offline: bool,
    pub force_download: bool,
    pub download_timeout: Duration,
}

impl Default for ProvisionOptions {
    fn default() -> Self {
        Self {
            offline: false,
            force_download: false,
            download_timeout: Duration::from_secs(30 * 60),
        }
    }
}

impl ProvisionOptions {
    pub fn offline() -> Self {
        Self {
            offline: true,
            ..Default::default()
        }
    }
}

/// Resolve every runtime needed by the plan: inject a compatible host runtime
/// when possible, otherwise fetch the smallest official Windows build.
pub fn provision(
    plan: &ResolutionPlan,
    matches: &[HostMatch],
    install_root: &Path,
    options: &ProvisionOptions,
) -> Result<ProvisionResult> {
    fsutil::ensure_dir(install_root)?;
    let mut result = ProvisionResult::default();
    result.estimated_download_bytes = estimate_download_bytes(matches);

    let mut kinds: BTreeSet<RuntimeKind> = plan
        .languages
        .iter()
        .filter_map(|language| language.runtime())
        .collect();
    kinds.extend(
        plan.requirements
            .iter()
            .map(|requirement| requirement.runtime),
    );

    for runtime in kinds {
        let host_match = matches.iter().find(|entry| entry.runtime == runtime);
        let compatible = host_match
            .map(|entry| entry.status == MatchStatus::Compatible)
            .unwrap_or(false);
        if compatible && !options.force_download {
            let host = host_match
                .and_then(|entry| entry.host.clone())
                .ok_or_else(|| {
                    Error::MissingRuntime(format!("{runtime:?} host entry disappeared"))
                })?;
            result.notes.push(format!(
                "Injected host {} {} from {} (no download needed).",
                runtime.display_name(),
                host.version,
                host.executable.display()
            ));
            result.runtimes.push(ProvisionedRuntime {
                runtime,
                version: host.version,
                home: host
                    .executable
                    .parent()
                    .map(|parent| {
                        if runtime == RuntimeKind::Java {
                            parent.parent().unwrap_or(parent).to_path_buf()
                        } else {
                            parent.to_path_buf()
                        }
                    })
                    .unwrap_or_else(|| host.executable.clone()),
                executable: host.executable,
                companions: host.companions,
                from_host: true,
                source: format!("host ({})", host.source),
            });
            continue;
        }

        if matches!(runtime, RuntimeKind::Rust | RuntimeKind::Go) {
            return Err(Error::MissingRuntime(format!(
                "{} is required but was not found in a compatible host installation. \
                 This port intentionally does not download the large Rust/Go toolchains; \
                 install {} and re-run, or use a host with it available.",
                runtime.display_name(),
                runtime.display_name()
            )));
        }

        if options.offline {
            return Err(Error::MissingRuntime(format!(
                "{} {} must be downloaded, but --offline was requested.",
                runtime.display_name(),
                host_match
                    .and_then(|entry| entry.minimum)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "runtime".to_string())
            )));
        }

        let minimum = host_match.and_then(|entry| entry.minimum);
        let fetched = match runtime {
            RuntimeKind::Python => {
                sources::fetch_python(minimum, install_root, &mut result.downloads, options)?
            }
            RuntimeKind::Node => {
                sources::fetch_node(minimum, install_root, &mut result.downloads, options)?
            }
            RuntimeKind::Java => {
                sources::fetch_java(minimum, install_root, &mut result.downloads, options)?
            }
            RuntimeKind::Rust | RuntimeKind::Go => unreachable!(),
        };
        result.notes.push(fetched.1);
        result.runtimes.push(fetched.0);
    }

    result.runtimes.sort_by_key(|runtime| runtime.runtime);
    Ok(result)
}

/// Estimate the download footprint without touching the network.
pub fn estimate_download_bytes(matches: &[HostMatch]) -> u64 {
    matches
        .iter()
        .filter(|entry| entry.status != MatchStatus::Compatible)
        .map(|entry| match entry.runtime {
            RuntimeKind::Python => 48 * 1024 * 1024,
            RuntimeKind::Node => 40 * 1024 * 1024,
            RuntimeKind::Java => 200 * 1024 * 1024,
            RuntimeKind::Rust => 400 * 1024 * 1024,
            RuntimeKind::Go => 150 * 1024 * 1024,
        })
        .sum()
}

#[derive(Clone, Debug, Default)]
pub struct PromotionReport {
    pub promoted: Vec<PathBuf>,
    pub path_dirs: Vec<PathBuf>,
    pub path_updated: bool,
    pub backup: Option<PathBuf>,
    pub notes: Vec<String>,
}

/// Keep & Promote: copy fetched toolchains into the user toolchain directory,
/// then idempotently prepend their directories to the *user* PATH. Host
/// runtimes are not copied. A backup of the previous PATH is written first.
pub fn promote(
    runtimes: &[ProvisionedRuntime],
    user_toolchains: &Path,
    update_path: bool,
) -> Result<PromotionReport> {
    fsutil::ensure_dir(user_toolchains)?;
    let mut report = PromotionReport::default();
    let mut path_dirs = BTreeSet::new();

    for runtime in runtimes {
        if runtime.from_host {
            report.notes.push(format!(
                "{} {} stays on the host; nothing to copy.",
                runtime.runtime.display_name(),
                runtime.version
            ));
            continue;
        }
        let target =
            user_toolchains.join(format!("{}-{}", runtime.runtime.as_str(), runtime.version));
        let relative = runtime
            .executable
            .strip_prefix(&runtime.home)
            .map(|path| path.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(runtime.executable.file_name().unwrap_or_default()));
        if !target.join(&relative).is_file() {
            if target.exists() {
                fsutil::remove_tree(&target)?;
            }
            let copy = fsutil::copy_tree_filtered(&runtime.home, &target, u64::MAX)?;
            report.notes.push(format!(
                "Promoted {} {} ({} files) to {}.",
                runtime.runtime.display_name(),
                runtime.version,
                copy.files,
                target.display()
            ));
        }
        report.promoted.push(target.clone());
        let path_dir = if runtime.runtime == RuntimeKind::Java {
            target.join("bin")
        } else {
            target.clone()
        };
        path_dirs.insert(path_dir);
    }

    report.path_dirs = path_dirs.into_iter().collect();
    if update_path && !report.path_dirs.is_empty() {
        let (updated, backup) = prepend_user_path(&report.path_dirs)?;
        report.path_updated = updated;
        report.backup = backup;
        if updated {
            report.notes.push(
                "User PATH updated idempotently. Open a new terminal to see the change; the current process PATH was not modified."
                    .to_string(),
            );
        }
    }
    Ok(report)
}

fn prepend_user_path(dirs: &[PathBuf]) -> Result<(bool, Option<PathBuf>)> {
    paths_ensure_dirs()?;
    let current = query_user_path().unwrap_or_default();
    let existing: Vec<String> = std::env::split_paths(&current)
        .map(|path| path.display().to_string().to_ascii_lowercase())
        .collect();
    let mut additions = Vec::new();
    for dir in dirs {
        let display = dir.display().to_string();
        if !existing
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&display))
        {
            additions.push(display);
        }
    }
    if additions.is_empty() {
        return Ok((false, None));
    }
    let backup = winorchestrator_core::paths::toolchains_dir()
        .join(format!("path-backup-{}.txt", unique_stamp()));
    let backup_text = format!(
        "# WinOrchestrator user PATH backup (UTF-8)\n# Written before Keep & Promote.\n{current}\n"
    );
    fsutil::write_utf8(&backup, &backup_text)?;

    let mut combined: Vec<String> = additions;
    if !current.trim().is_empty() {
        combined.push(current);
    }
    let new_path = combined.join(";");
    let output = process::run_capture(
        "reg.exe",
        &[
            "add",
            "HKCU\\Environment",
            "/v",
            "Path",
            "/t",
            "REG_EXPAND_SZ",
            "/d",
            &new_path,
            "/f",
        ],
        &RunOptions::default().timeout(Duration::from_secs(20)),
    );
    if !output.success {
        return Err(Error::Message(format!(
            "could not update the user PATH: {}",
            output.combined()
        )));
    }
    Ok((true, Some(backup)))
}

fn query_user_path() -> Option<String> {
    let output = process::run_capture(
        "reg.exe",
        &["query", "HKCU\\Environment", "/v", "Path"],
        &RunOptions::default().timeout(Duration::from_secs(20)),
    );
    if !output.success {
        return None;
    }
    let line = output
        .stdout
        .lines()
        .find(|line| line.contains("REG_EXPAND_SZ") || line.contains("REG_SZ"))?;
    let marker = if line.contains("REG_EXPAND_SZ") {
        "REG_EXPAND_SZ"
    } else {
        "REG_SZ"
    };
    Some(
        line.split_once(marker)
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default(),
    )
}

fn paths_ensure_dirs() -> Result<()> {
    fsutil::ensure_dir(&winorchestrator_core::paths::toolchains_dir())
}

fn unique_stamp() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos}")
}

pub fn runtime_path_entries(runtimes: &[ProvisionedRuntime]) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    for runtime in runtimes {
        dirs.insert(runtime.path_dir());
    }
    dirs.into_iter().collect()
}

/// Remove downloaded toolchains from the sandbox (Destroy & Clean path).
pub fn remove_sandbox_tools(install_root: &Path) -> Result<()> {
    if install_root.exists() {
        fsutil::remove_tree(install_root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_are_zero_when_host_matches() {
        let matches = vec![HostMatch {
            runtime: RuntimeKind::Python,
            requirement: Some(">=3.10".to_string()),
            minimum: Some(Version::new(3, 10, 0)),
            host: None,
            status: MatchStatus::Compatible,
            detail: String::new(),
        }];
        assert_eq!(estimate_download_bytes(&matches), 0);
    }

    #[test]
    fn estimates_nonzero_when_missing() {
        let matches = vec![HostMatch {
            runtime: RuntimeKind::Node,
            requirement: Some(">=18".to_string()),
            minimum: Some(Version::new(18, 0, 0)),
            host: None,
            status: MatchStatus::NoHostRuntime,
            detail: String::new(),
        }];
        assert_eq!(estimate_download_bytes(&matches), 40 * 1024 * 1024);
    }
}
