use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::{Error, HostMatch, MatchStatus, ResolutionPlan, Result, RuntimeKind, Version};

mod download;
mod sources;

pub use download::{download_verified, fetch_text_cached, sha256_matches};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadRecord { pub runtime: RuntimeKind, pub version: String, pub file: PathBuf, pub bytes: u64, pub sha256: String, pub source: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvisionedRuntime { pub runtime: RuntimeKind, pub version: Version, pub home: PathBuf, pub executable: PathBuf, pub companions: BTreeMap<String, PathBuf>, pub from_host: bool, pub source: String }

impl ProvisionedRuntime {
    pub fn path_dir(&self) -> PathBuf {
        match self.runtime { RuntimeKind::Java => self.home.join("bin"), _ => self.home.clone() }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProvisionResult { pub runtimes: Vec<ProvisionedRuntime>, pub downloads: Vec<DownloadRecord>, pub estimated_download_bytes: u64, pub notes: Vec<String>, pub warnings: Vec<String> }

#[derive(Clone, Debug)]
pub struct ProvisionOptions { pub offline: bool, pub force_download: bool, pub download_timeout: Duration }

impl Default for ProvisionOptions { fn default() -> Self { Self { offline: false, force_download: false, download_timeout: Duration::from_secs(30 * 60) } } }
impl ProvisionOptions { pub fn offline() -> Self { Self { offline: true, ..Default::default() } } }

pub fn provision(plan: &ResolutionPlan, matches: &[HostMatch], install_root: &Path, options: &ProvisionOptions) -> Result<ProvisionResult> {
    fsutil::ensure_dir(install_root)?;
    let mut result = ProvisionResult::default();
    result.estimated_download_bytes = estimate_download_bytes(matches);

    let mut kinds: BTreeSet<RuntimeKind> = plan.languages.iter().filter_map(|l| l.runtime()).collect();
    kinds.extend(plan.requirements.iter().map(|r| r.runtime));

    for runtime in kinds {
        let host_match = matches.iter().find(|e| e.runtime == runtime);
        let compatible = host_match.map(|e| e.status == MatchStatus::Compatible).unwrap_or(false);
        if compatible && !options.force_download {
            let host = host_match.and_then(|e| e.host.clone()).ok_or_else(|| Error::MissingRuntime(format!("{runtime:?} host entry disappeared")))?;
            result.notes.push(format!("Injected host {} {} from {} (no download needed).", runtime.display_name(), host.version, host.executable.display()));
            result.runtimes.push(ProvisionedRuntime { runtime, version: host.version, home: host.executable.parent().map(|p| if runtime == RuntimeKind::Java { p.parent().unwrap_or(p).to_path_buf() } else { p.to_path_buf() }).unwrap_or_else(|| host.executable.clone()), executable: host.executable, companions: host.companions, from_host: true, source: format!("host ({})", host.source) });
            continue;
        }

        if matches!(runtime, RuntimeKind::Rust | RuntimeKind::Go) {
            return Err(Error::MissingRuntime(format!("{} is required but not installed locally. Please install it.", runtime.display_name())));
        }

        if options.offline { return Err(Error::MissingRuntime(format!("{} requires download, but --offline requested.", runtime.display_name()))); }

        let minimum = host_match.and_then(|e| e.minimum);
        let fetched = match runtime {
            RuntimeKind::Python => sources::fetch_python(minimum, install_root, &mut result.downloads, options)?,
            RuntimeKind::Node => sources::fetch_node(minimum, install_root, &mut result.downloads, options)?,
            RuntimeKind::Java => sources::fetch_java(minimum, install_root, &mut result.downloads, options)?,
            RuntimeKind::Rust | RuntimeKind::Go => unreachable!(),
        };
        result.notes.push(fetched.1);
        result.runtimes.push(fetched.0);
    }
    result.runtimes.sort_by_key(|r| r.runtime);
    Ok(result)
}

pub fn estimate_download_bytes(matches: &[HostMatch]) -> u64 {
    matches.iter().filter(|e| e.status != MatchStatus::Compatible).map(|e| match e.runtime { RuntimeKind::Python => 48 * 1024 * 1024, RuntimeKind::Node => 40 * 1024 * 1024, RuntimeKind::Java => 200 * 1024 * 1024, RuntimeKind::Rust => 400 * 1024 * 1024, RuntimeKind::Go => 150 * 1024 * 1024 }).sum()
}

#[derive(Clone, Debug, Default)]
pub struct PromotionReport { pub promoted: Vec<PathBuf>, pub path_dirs: Vec<PathBuf>, pub path_updated: bool, pub backup: Option<PathBuf>, pub notes: Vec<String> }

pub fn promote(runtimes: &[ProvisionedRuntime], user_toolchains: &Path, update_path: bool) -> Result<PromotionReport> {
    fsutil::ensure_dir(user_toolchains)?;
    let mut report = PromotionReport::default();
    let mut path_dirs = BTreeSet::new();

    for runtime in runtimes {
        if runtime.from_host {
            report.notes.push(format!("{} {} stays on the host.", runtime.runtime.display_name(), runtime.version));
            continue;
        }
        let target = user_toolchains.join(format!("{}-{}", runtime.runtime.as_str(), runtime.version));
        let relative = runtime.executable.strip_prefix(&runtime.home).map(|p| p.to_path_buf()).unwrap_or_else(|_| PathBuf::from(runtime.executable.file_name().unwrap_or_default()));
        
        if !target.join(&relative).is_file() {
            if target.exists() { fsutil::remove_tree(&target)?; }
            fsutil::copy_tree_filtered(&runtime.home, &target, u64::MAX)?;
            report.notes.push(format!("Promoted {} {} to {}.", runtime.runtime.display_name(), runtime.version, target.display()));
        }
        report.promoted.push(target.clone());
        path_dirs.insert(if runtime.runtime == RuntimeKind::Java { target.join("bin") } else { target.clone() });
    }

    report.path_dirs = path_dirs.into_iter().collect();
    if update_path && !report.path_dirs.is_empty() {
        let (updated, backup) = prepend_user_path(&report.path_dirs)?;
        report.path_updated = updated;
        report.backup = backup;
        if updated { report.notes.push("User PATH updated in ~/.bashrc idempotently. Run `source ~/.bashrc` to apply.".to_string()); }
    }
    Ok(report)
}

const PATH_BLOCK_BEGIN: &str = "# >>> LinuxOrchestrator Keep & Promote >>>";
const PATH_BLOCK_END: &str = "# <<< LinuxOrchestrator Keep & Promote <<<";

/// Idempotently add toolchain directories to `~/.bashrc`. The managed block is
/// rewritten as a union, so repeated Keep runs never duplicate PATH entries.
fn prepend_user_path(dirs: &[PathBuf]) -> Result<(bool, Option<PathBuf>)> {
    if dirs.is_empty() {
        return Ok((false, None));
    }
    let bashrc = std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".bashrc"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/.bashrc"));
    let backup = linuxorchestrator_core::paths::toolchains_dir()
        .join(format!("bashrc-backup-{}.txt", unique_stamp()));
    let changed = update_bashrc_file(&bashrc, dirs, Some(&backup))?;
    Ok((changed, changed.then_some(backup)))
}

/// Rewrite the managed block as a union of existing and new directories.
/// Returns `true` when the file changed.
fn update_bashrc_file(bashrc: &Path, dirs: &[PathBuf], backup: Option<&Path>) -> Result<bool> {
    if dirs.is_empty() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(bashrc).unwrap_or_default();

    let mut managed: BTreeSet<String> = BTreeSet::new();
    let mut content = original.clone();
    if let Some(start) = content.find(PATH_BLOCK_BEGIN) {
        if let Some(relative_end) = content[start..].find(PATH_BLOCK_END) {
            let end = start + relative_end + PATH_BLOCK_END.len();
            managed.extend(parse_path_block(&content[start..end]));
            content.replace_range(start..end, "");
        }
    }

    let before = managed.len();
    for dir in dirs {
        managed.insert(dir.display().to_string());
    }
    if managed.len() == before {
        return Ok(false);
    }

    if let Some(path) = backup {
        fsutil::write_utf8(path, &original)?;
    }
    let new_content = format!(
        "{}\n\n{PATH_BLOCK_BEGIN}\nexport PATH=\"{}:$PATH\"\n{PATH_BLOCK_END}\n",
        content.trim_end(),
        managed.into_iter().collect::<Vec<_>>().join(":")
    );
    fsutil::write_utf8(bashrc, &new_content)?;
    Ok(true)
}

fn parse_path_block(block: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    for line in block.lines() {
        let Some(rest) = line.trim().strip_prefix("export PATH=\"") else {
            continue;
        };
        let Some(value) = rest.strip_suffix(":$PATH\"") else {
            continue;
        };
        for dir in value.split(':') {
            if !dir.trim().is_empty() {
                dirs.push(dir.to_string());
            }
        }
    }
    dirs
}

fn unique_stamp() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos}")
}

pub fn runtime_path_entries(runtimes: &[ProvisionedRuntime]) -> Vec<PathBuf> {
    runtimes.iter().map(|r| r.path_dir()).collect()
}

pub fn remove_sandbox_tools(install_root: &Path) -> Result<()> {
    if install_root.exists() { fsutil::remove_tree(install_root)?; }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "linuxorchestrator-provisioner-{name}-{}",
            unique_stamp()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(".bashrc")
    }

    #[test]
    fn path_block_parses_directories() {
        let block = format!("{PATH_BLOCK_BEGIN}\nexport PATH=\"/a:/b:$PATH\"\n{PATH_BLOCK_END}");
        assert_eq!(parse_path_block(&block), vec!["/a", "/b"]);
    }

    #[test]
    fn bashrc_update_is_idempotent_and_preserves_user_content() {
        let bashrc = temp_file("idempotent");
        std::fs::write(&bashrc, "export EDITOR=vi\n").unwrap();
        let dirs = vec![PathBuf::from("/opt/tool-a"), PathBuf::from("/opt/tool-b")];

        assert!(update_bashrc_file(&bashrc, &dirs, None).unwrap());
        let first = std::fs::read_to_string(&bashrc).unwrap();
        assert!(first.contains("export EDITOR=vi"));
        assert_eq!(first.matches(PATH_BLOCK_BEGIN).count(), 1);
        assert!(first.contains("/opt/tool-a:/opt/tool-b"));

        assert!(!update_bashrc_file(&bashrc, &dirs, None).unwrap());
        assert_eq!(std::fs::read_to_string(&bashrc).unwrap(), first);

        assert!(update_bashrc_file(&bashrc, &[PathBuf::from("/opt/tool-c")], None).unwrap());
        let merged = std::fs::read_to_string(&bashrc).unwrap();
        assert_eq!(merged.matches(PATH_BLOCK_BEGIN).count(), 1);
        assert!(merged.contains("/opt/tool-a:/opt/tool-b:/opt/tool-c"));
    }
}
