use sha2::{Digest, Sha256};
use std::env;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Root for all state this tool creates on Windows.
///
/// Default: `%LOCALAPPDATA%\WinOrchestrator`
pub fn app_root() -> PathBuf {
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("WinOrchestrator");
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join("WinOrchestrator");
    }
    env::temp_dir().join("WinOrchestrator")
}

pub fn cache_dir() -> PathBuf {
    app_root().join("cache")
}

pub fn workspaces_dir() -> PathBuf {
    cache_dir().join("workspaces")
}

pub fn downloads_dir() -> PathBuf {
    cache_dir().join("downloads")
}

pub fn sandboxes_dir() -> PathBuf {
    app_root().join("sandboxes")
}

pub fn toolchains_dir() -> PathBuf {
    app_root().join("toolchains")
}

pub fn snapshots_dir() -> PathBuf {
    app_root().join("snapshots")
}

pub fn logs_dir() -> PathBuf {
    app_root().join("logs")
}

pub fn locks_dir() -> PathBuf {
    app_root().join("locks")
}

pub fn ensure_app_dirs() -> crate::Result<()> {
    for dir in [
        app_root(),
        cache_dir(),
        workspaces_dir(),
        downloads_dir(),
        sandboxes_dir(),
        toolchains_dir(),
        snapshots_dir(),
        logs_dir(),
        locks_dir(),
    ] {
        std::fs::create_dir_all(&dir).map_err(|source| crate::Error::Io {
            context: format!(" while creating {}", dir.display()),
            source,
        })?;
    }
    Ok(())
}

fn sanitize_component(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('.').trim_matches('_');
    let trimmed = if trimmed.is_empty() {
        "project"
    } else {
        trimmed
    };
    trimmed.chars().take(80).collect()
}

/// Best-effort repository name for a URL or local path. Always safe to use as
/// a Windows directory name.
pub fn repo_name_from_target(target: &str) -> String {
    let cleaned = target.trim().trim_end_matches(['/', '\\']);
    let without_git = cleaned.strip_suffix(".git").unwrap_or(cleaned);
    let last = without_git
        .rsplit(['/', '\\', ':'])
        .find(|part| !part.is_empty() && *part != ".git")
        .unwrap_or("project");
    sanitize_component(last)
}

pub fn stable_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub fn unique_sandbox_id(repo_name: &str) -> String {
    format!(
        "{}-{}-{}",
        sanitize_component(repo_name),
        timestamp_compact(),
        std::process::id()
    )
}

/// Lexical containment check. Both paths are normalised to comparable
/// components, so `C:\Cache\Sandbox2` is not considered inside
/// `C:\Cache\Sandbox`.
pub fn is_within(parent: &Path, child: &Path) -> bool {
    let parent = comparable_components(parent);
    let child = comparable_components(child);
    !parent.is_empty() && child.len() >= parent.len() && child[..parent.len()] == parent[..]
}

fn comparable_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
            Component::RootDir => Some("\\".to_string()),
            Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

/// The volume root used for disk-space checks (`C:\` or a UNC share root).
pub fn volume_root(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if text.starts_with("\\\\") {
        let mut parts = text.split('\\').filter(|p| !p.is_empty());
        if let (Some(server), Some(share)) = (parts.next(), parts.next()) {
            return PathBuf::from(format!("\\\\{server}\\{share}\\"));
        }
    }
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        return PathBuf::from(&text[..3]);
    }
    PathBuf::from("C:\\")
}

pub fn default_snapshot_file(repo_name: &str, cwd: &Path) -> PathBuf {
    cwd.join(format!("{}.envspec.utf8", sanitize_component(repo_name)))
}

fn days_to_civil(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// UTC timestamp in ISO-8601 form, implemented without a date crate.
pub fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = days_to_civil(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub fn timestamp_compact() -> String {
    let iso = iso8601_now();
    iso.chars()
        .filter(|c| c.is_ascii_digit())
        .take(14)
        .collect()
}

/// Join a user-relative path onto a root, rejecting `..` escapes and absolute
/// paths supplied by a repository.
pub fn safe_join(root: &Path, relative: &str) -> crate::Result<PathBuf> {
    let relative = relative.replace('\\', "/");
    let candidate = root.join(relative.trim_start_matches('/'));
    if !is_within(root, &candidate) {
        return Err(crate::Error::PathEscape(candidate));
    }
    Ok(candidate)
}

/// Convert a path to a UTF-8 display string without losing the original path.
pub fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_are_windows_safe() {
        assert_eq!(
            repo_name_from_target("https://github.com/acme/cool-repo.git"),
            "cool-repo"
        );
        assert_eq!(
            repo_name_from_target("C:\\work\\My Project\\"),
            "My_Project"
        );
        assert_eq!(
            repo_name_from_target("git@github.com:acme/thing.git"),
            "thing"
        );
    }

    #[test]
    fn containment_is_component_based() {
        let root = PathBuf::from("C:\\Cache\\Sandbox");
        assert!(is_within(&root, &root.join("workspace\\main.py")));
        assert!(!is_within(&root, &PathBuf::from("C:\\Cache\\Sandbox2\\x")));
    }

    #[test]
    fn timestamps_are_stable_shapes() {
        let iso = iso8601_now();
        assert_eq!(iso.len(), 20);
        assert!(iso.ends_with('Z'));
        assert_eq!(timestamp_compact().len(), 14);
    }
}
