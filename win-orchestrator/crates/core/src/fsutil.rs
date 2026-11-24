use crate::error::{Error, IoContext, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "env",
    "node_modules",
    "target",
    "dist",
    "build",
    ".gradle",
    ".idea",
    ".vs",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".next",
    ".nuxt",
    "coverage",
];

/// Write text as strict UTF-8 with no BOM. The snapshot format depends on this.
pub fn write_utf8(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }
    let mut file = fs::File::create(path).with_path(path)?;
    file.write_all(content.as_bytes()).with_path(path)?;
    file.flush().with_path(path)?;
    Ok(())
}

/// Read a UTF-8 file, tolerating an optional BOM but rejecting invalid UTF-8
/// with an actionable message.
pub fn read_utf8_strict(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_path(path)?;
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        &bytes[..]
    };
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        Error::Message(format!(
            "{} is not valid UTF-8. WinOrchestrator restore files are strictly UTF-8.",
            path.display()
        ))
    })
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_path(path)
}

/// Copy a repository tree while skipping build outputs, VCS data and caches.
/// Files larger than `max_file_bytes` are skipped (reported back) to keep
/// staging quick; generated assets can be rebuilt inside the sandbox.
pub fn copy_tree_filtered(
    source: &Path,
    destination: &Path,
    max_file_bytes: u64,
) -> Result<CopyReport> {
    if !source.exists() {
        return Err(Error::Message(format!(
            "source does not exist: {}",
            source.display()
        )));
    }
    ensure_dir(destination)?;
    let mut report = CopyReport::default();
    let walker = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("__pycache__")
                && !SKIP_DIR_NAMES.iter().any(|skip| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(skip)
                })
        });

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                report.warnings.push(format!("walk warning: {err}"));
                continue;
            }
        };
        let relative = match entry.path().strip_prefix(source) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
            continue;
        }
        if entry.file_type().is_symlink() {
            report
                .warnings
                .push(format!("skipped symlink {}", entry.path().display()));
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                report
                    .warnings
                    .push(format!("could not stat {}: {err}", entry.path().display()));
                continue;
            }
        };
        if metadata.len() > max_file_bytes {
            report.skipped_large += 1;
            report.warnings.push(format!(
                "skipped {} ({} > {})",
                relative.display(),
                human_bytes(metadata.len()),
                human_bytes(max_file_bytes)
            ));
            continue;
        }
        if let Some(parent) = target.parent() {
            ensure_dir(parent)?;
        }
        fs::copy(entry.path(), &target).with_path(entry.path())?;
        report.files += 1;
        report.bytes += metadata.len();
    }
    Ok(report)
}

#[derive(Clone, Debug, Default)]
pub struct CopyReport {
    pub files: u64,
    pub bytes: u64,
    pub skipped_large: u64,
    pub warnings: Vec<String>,
}

/// Remove a directory tree. Junctions/symlinks are unlinked, never traversed,
/// so a sandbox cannot delete a host cache it points at.
pub fn remove_tree(path: &Path) -> Result<()> {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    let file_type = metadata.file_type();
    let is_link = file_type.is_symlink();
    if is_link {
        #[cfg(windows)]
        {
            if file_type.is_dir() {
                if fs::remove_dir(path).is_err() {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "rmdir", &path.display().to_string()])
                        .status();
                }
                return Ok(());
            }
        }
        fs::remove_file(path).with_path(path)?;
        return Ok(());
    }
    if file_type.is_dir() {
        fs::remove_dir_all(path).with_path(path)?;
    } else {
        fs::remove_file(path).with_path(path)?;
    }
    Ok(())
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_path(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).with_path(path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub fn directory_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

/// Create a Windows junction using `mklink /J` (no admin or developer mode
/// required, unlike symlinks). Returns an error with the command output when
/// it fails so the caller can fall back to copying.
pub fn create_junction(link: &Path, target: &Path) -> Result<()> {
    if link.exists() {
        return Ok(());
    }
    if let Some(parent) = link.parent() {
        ensure_dir(parent)?;
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.display().to_string(),
                &target.display().to_string(),
            ])
            .output()
            .with_path(link)?;
        if output.status.success() {
            return Ok(());
        }
        return Err(Error::Message(format!(
            "could not create junction {} -> {}: {}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).with_path(link)?;
        Ok(())
    }
}

pub fn read_dir_sorted(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(path)
        .with_path(path)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("winorchestrator-test-{name}-{stamp}"))
    }

    #[test]
    fn utf8_roundtrip_and_bom_tolerance() {
        let dir = scratch("utf8");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.envspec.utf8");
        write_utf8(&file, "héllo ✓\n").unwrap();
        assert_eq!(read_utf8_strict(&file).unwrap(), "héllo ✓\n");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(b"ok");
        fs::write(&file, with_bom).unwrap();
        assert_eq!(read_utf8_strict(&file).unwrap(), "ok");
        remove_tree(&dir).unwrap();
    }

    #[test]
    fn copy_skips_heavy_directories() {
        let dir = scratch("copy");
        let src = dir.join("src");
        fs::create_dir_all(src.join("node_modules")).unwrap();
        fs::create_dir_all(src.join("pkg")).unwrap();
        fs::write(src.join("pkg").join("main.py"), "print(1)").unwrap();
        fs::write(src.join("node_modules").join("x.js"), "x").unwrap();
        let dst = dir.join("dst");
        let report = copy_tree_filtered(&src, &dst, 1024 * 1024).unwrap();
        assert!(dst.join("pkg").join("main.py").exists());
        assert!(!dst.join("node_modules").exists());
        assert_eq!(report.files, 1);
        remove_tree(&dir).unwrap();
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }
}
