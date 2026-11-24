use crate::error::{Error, IoContext, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SKIP_DIR_NAMES: &[&str] = &[".git", ".venv", "venv", "env", "node_modules", "target", "dist", "build", "__pycache__", ".pytest_cache"];

pub fn write_utf8(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).with_path(parent)?; }
    let mut file = fs::File::create(path).with_path(path)?;
    file.write_all(content.as_bytes()).with_path(path)?;
    file.flush().with_path(path)?;
    Ok(())
}

pub fn read_utf8_strict(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_path(path)?;
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { &bytes[3..] } else { &bytes[..] };
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::Message(format!("{} is not valid UTF-8.", path.display())))
}

pub fn ensure_dir(path: &Path) -> Result<()> { fs::create_dir_all(path).with_path(path) }

pub fn copy_tree_filtered(source: &Path, destination: &Path, max_file_bytes: u64) -> Result<CopyReport> {
    if !source.exists() { return Err(Error::Message(format!("source does not exist: {}", source.display()))); }
    ensure_dir(destination)?;
    let mut report = CopyReport::default();
    let walker = WalkDir::new(source).follow_links(false).into_iter().filter_entry(|entry| {
        if entry.depth() == 0 { return true; }
        !SKIP_DIR_NAMES.iter().any(|skip| entry.file_name().to_string_lossy().eq_ignore_ascii_case(skip))
    });

    for entry in walker.filter_map(|e| e.ok()) {
        let relative = match entry.path().strip_prefix(source) { Ok(r) => r, Err(_) => continue };
        if relative.as_os_str().is_empty() { continue; }
        let target = destination.join(relative);
        
        if entry.file_type().is_dir() { ensure_dir(&target)?; continue; }
        if entry.file_type().is_symlink() { report.warnings.push(format!("skipped symlink {}", entry.path().display())); continue; }
        
        if let Ok(metadata) = entry.metadata() {
            if metadata.len() > max_file_bytes { report.skipped_large += 1; continue; }
            if let Some(parent) = target.parent() { ensure_dir(parent)?; }
            fs::copy(entry.path(), &target).with_path(entry.path())?;
            report.files += 1; report.bytes += metadata.len();
        }
    }
    Ok(report)
}

#[derive(Clone, Debug, Default)]
pub struct CopyReport { pub files: u64, pub bytes: u64, pub skipped_large: u64, pub warnings: Vec<String> }

pub fn remove_tree(path: &Path) -> Result<()> {
    if !path.exists() && fs::symlink_metadata(path).is_err() { return Ok(()); }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() { fs::remove_file(path).with_path(path)?; } 
        else { fs::remove_dir_all(path).with_path(path)?; }
    }
    Ok(())
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 { value /= 1024.0; unit += 1; }
    if unit == 0 { format!("{} {}", bytes, UNITS[unit]) } else { format!("{value:.1} {}", UNITS[unit]) }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_path(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).with_path(path)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn file_size(path: &Path) -> u64 { fs::metadata(path).map(|m| m.len()).unwrap_or(0) }
pub fn directory_size(path: &Path) -> u64 { WalkDir::new(path).follow_links(false).into_iter().filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum() }

pub fn create_junction(link: &Path, target: &Path) -> Result<()> {
    if link.exists() { return Ok(()); }
    if let Some(parent) = link.parent() { ensure_dir(parent)?; }
    std::os::unix::fs::symlink(target, link).with_path(link)?;
    Ok(())
}

pub fn read_dir_sorted(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(path).with_path(path)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    Ok(entries)
}