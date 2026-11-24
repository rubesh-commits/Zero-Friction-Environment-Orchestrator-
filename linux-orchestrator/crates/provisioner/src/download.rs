use crate::ProvisionOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;
use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::process::{self, RunOptions};
use linuxorchestrator_core::{Error, Result};

#[derive(Clone, Debug)]
pub struct Downloaded {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub reused: bool,
}

pub enum HashExpectation<'a> {
    Inline(&'a str),
    SumsUrl { url: &'a str, filename: &'a str },
}

pub fn download_verified(
    url: &str,
    filename: &str,
    expectation: HashExpectation<'_>,
    downloads_dir: &Path,
    options: &ProvisionOptions,
) -> Result<Downloaded> {
    fsutil::ensure_dir(downloads_dir)?;
    let target = downloads_dir.join(filename);
    let expected = match expectation {
        HashExpectation::Inline(hash) => normalize_hash(hash)?,
        HashExpectation::SumsUrl { url, filename } => {
            let cache = downloads_dir.join(format!("{}.sha256sums", sanitize_file_name(url)));
            let text = fetch_text_cached(url, &cache, options.offline)?;
            parse_checksums(&text, filename).ok_or_else(|| {
                Error::Download(format!("the checksum list at {url} did not contain {filename}"))
            })?
        }
    };

    if target.is_file() {
        let actual = fsutil::sha256_file(&target)?;
        if actual.eq_ignore_ascii_case(&expected) {
            return Ok(Downloaded { bytes: fsutil::file_size(&target), path: target, sha256: actual, reused: true });
        }
        fsutil::remove_tree(&target)?;
    }
    if options.offline {
        return Err(Error::Download(format!("{} is not cached and --offline was requested", filename)));
    }

    let part = target.with_extension("part");
    if part.exists() { let _ = fsutil::remove_tree(&part); }
    
    println!("Downloading {filename}...");
    download_raw(url, &part, options.download_timeout)?;
    
    let actual = fsutil::sha256_file(&part)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = fsutil::remove_tree(&part);
        return Err(Error::Checksum(format!("{filename}: expected {expected}, got {actual}")));
    }
    std::fs::rename(&part, &target).map_err(|source| Error::Io { context: format!("finalising {}", target.display()), source })?;
    
    Ok(Downloaded { bytes: fsutil::file_size(&target), path: target, sha256: actual, reused: false })
}

pub fn fetch_text_cached(url: &str, cache_file: &Path, offline: bool) -> Result<String> {
    if cache_file.is_file() {
        if let Ok(text) = fsutil::read_utf8_strict(cache_file) {
            if !text.trim().is_empty() { return Ok(text); }
        }
    }
    if offline { return Err(Error::Download(format!("cannot fetch {url} in offline mode"))); }
    
    let text = fetch_text(url)?;
    if let Some(parent) = cache_file.parent() { fsutil::ensure_dir(parent)?; }
    fsutil::write_utf8(cache_file, &text)?;
    Ok(text)
}

fn fetch_text(url: &str) -> Result<String> {
    let outcome = process::run_capture("curl", &["--silent", "--show-error", "--location", "--fail", "--retry", "2", "--connect-timeout", "20", url], &RunOptions::default().timeout(Duration::from_secs(120)));
    if outcome.success { Ok(outcome.stdout) } else { Err(Error::Download(format!("could not fetch {url}: {}", outcome.combined()))) }
}

fn download_raw(url: &str, destination: &Path, timeout: Duration) -> Result<()> {
    if let Some(parent) = destination.parent() { fsutil::ensure_dir(parent)?; }
    let outcome = process::run_capture("curl", &["--location", "--fail", "--retry", "3", "--connect-timeout", "30", "--output", &destination.display().to_string(), url], &RunOptions::default().timeout(timeout));
    if outcome.success && destination.is_file() { Ok(()) } else {
        let _ = fsutil::remove_tree(destination);
        Err(Error::Download(format!("curl failed for {url}: {}", outcome.combined())))
    }
}

pub fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    fsutil::ensure_dir(destination)?;
    let archive_str = archive.display().to_string();
    
    if archive_str.ends_with(".zip") {
        let outcome = process::run_capture("unzip", &["-q", &archive_str, "-d", &destination.display().to_string()], &RunOptions::default().timeout(Duration::from_secs(900)));
        if outcome.success { return Ok(()); }
        Err(Error::Message(format!("could not unzip {}: {}", archive_str, outcome.combined())))
    } else {
        let outcome = process::run_capture("tar", &["-xf", &archive_str, "-C", &destination.display().to_string()], &RunOptions::default().timeout(Duration::from_secs(900)));
        if outcome.success { return Ok(()); }
        Err(Error::Message(format!("could not tar extract {}: {}", archive_str, outcome.combined())))
    }
}

pub fn sha256_matches(path: &Path, expected: &str) -> Result<bool> {
    Ok(fsutil::sha256_file(path)?.eq_ignore_ascii_case(expected))
}

pub fn parse_checksums(text: &str, filename: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next().unwrap_or("").trim_start_matches('*');
        if name == filename || name.ends_with(&format!("/{filename}")) {
            if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) { return Some(hash.to_ascii_lowercase()); }
        }
    }
    None
}

fn normalize_hash(hash: &str) -> Result<String> {
    let hash = hash.trim().to_ascii_lowercase();
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) { Ok(hash) } else { Err(Error::Checksum(format!("invalid SHA-256 value: {hash}"))) }
}

fn sanitize_file_name(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { out.push(ch); } else { out.push('_'); }
    }
    out.chars().take(120).collect()
}