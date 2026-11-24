use crate::download::{download_verified, extract_archive, fetch_text_cached, HashExpectation};
use crate::{DownloadRecord, ProvisionOptions, ProvisionedRuntime};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::{Error, Result, RuntimeKind, Version};

const PBS_TAG: &str = "20261123";
const PBS_BASE: &str = "https://github.com/astral-sh/python-build-standalone/releases/download";
const PYTHON_BUILDS: &[(u32, u32, u32)] = &[(3, 11, 18), (3, 12, 16), (3, 13, 18), (3, 14, 10)];
const NODE_DEFAULT: Version = Version::new(24, 22, 3);

pub fn fetch_python(
    minimum: Option<Version>, install_root: &Path, downloads: &mut Vec<DownloadRecord>, options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let version = choose_python(minimum)?;
    // Fetching the Linux GNU build instead of Windows MSVC
    let asset = format!("cpython-{version}+{PBS_TAG}-x86_64-unknown-linux-gnu-install_only.tar.gz");
    let url = format!("{PBS_BASE}/{PBS_TAG}/{asset}");
    let sums_url = format!("{PBS_BASE}/{PBS_TAG}/SHA256SUMS");
    
    let downloaded = download_verified(&url, &asset, HashExpectation::SumsUrl { url: &sums_url, filename: &asset }, &linuxorchestrator_core::paths::downloads_dir(), options)?;
    
    downloads.push(DownloadRecord { runtime: RuntimeKind::Python, version: version.to_string(), file: downloaded.path.clone(), bytes: downloaded.bytes, sha256: downloaded.sha256.clone(), source: url.clone() });
    
    let target = install_root.join(format!("python-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "python3")?;
    let home = executable.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| target.clone());
    
    let mut companions = BTreeMap::new();
    for name in ["pip", "pip3", "python"] {
        if let Some(path) = find_file(&home, name, 3) { companions.insert(name.to_string(), path); }
    }
    
    let note = format!("Fetched Python {version} for Linux from python-build-standalone (SHA-256 verified).");
    Ok((ProvisionedRuntime { runtime: RuntimeKind::Python, version, home, executable, companions, from_host: false, source: format!("python-build-standalone {PBS_TAG}") }, note))
}

pub fn fetch_node(
    minimum: Option<Version>, install_root: &Path, downloads: &mut Vec<DownloadRecord>, options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let version = choose_node(minimum, options)?;
    // Node.js Linux tarball
    let asset = format!("node-v{version}-linux-x64.tar.xz");
    let url = format!("https://nodejs.org/dist/v{version}/{asset}");
    let sums_url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    
    let downloaded = download_verified(&url, &asset, HashExpectation::SumsUrl { url: &sums_url, filename: &asset }, &linuxorchestrator_core::paths::downloads_dir(), options)?;
    downloads.push(DownloadRecord { runtime: RuntimeKind::Node, version: version.to_string(), file: downloaded.path.clone(), bytes: downloaded.bytes, sha256: downloaded.sha256.clone(), source: url.clone() });
    
    let target = install_root.join(format!("node-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "node")?;
    let home = executable.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| target.clone());
    
    let mut companions = BTreeMap::new();
    for name in ["npm", "npx", "corepack"] {
        if let Some(path) = find_file(&home, name, 2) { companions.insert(name.to_string(), path); }
    }
    Ok((ProvisionedRuntime { runtime: RuntimeKind::Node, version, home, executable, companions, from_host: false, source: "nodejs.org dist".to_string() }, format!("Fetched Node.js {version} from nodejs.org (SHA-256 verified).")))
}

pub fn fetch_java(
    minimum: Option<Version>, install_root: &Path, downloads: &mut Vec<DownloadRecord>, options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let feature = choose_java_feature(minimum);
    // Explicit os=linux for the JDK payload
    let api = format!("https://api.adoptium.net/v3/assets/latest/{feature}/hotspot?architecture=x64&image_type=jdk&os=linux&vendor=eclipse&page=0&page_size=1");
    let cache = linuxorchestrator_core::paths::downloads_dir().join(format!("adoptium-{feature}.json"));
    
    let text = fetch_text_cached(&api, &cache, options.offline)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let item = value.get(0).ok_or_else(|| Error::Download(format!("Adoptium returned no JDK build for Java {feature}")))?;
    
    let package = &item["binary"]["package"];
    let link = package["link"].as_str().unwrap();
    let checksum = package["checksum"].as_str().unwrap();
    let filename = package["name"].as_str().map(|n| n.to_string()).unwrap_or_else(|| format!("temurin-{feature}-jdk-linux-x64.tar.gz"));
    let version = item["version"]["semver"].as_str().and_then(Version::parse).unwrap_or_else(|| Version::new(feature, 0, 0));

    let downloaded = download_verified(link, &filename, HashExpectation::Inline(checksum), &linuxorchestrator_core::paths::downloads_dir(), options)?;
    downloads.push(DownloadRecord { runtime: RuntimeKind::Java, version: version.to_string(), file: downloaded.path.clone(), bytes: downloaded.bytes, sha256: downloaded.sha256.clone(), source: link.to_string() });
    
    let target = install_root.join(format!("java-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "java")?;
    let home = executable.parent().and_then(|b| b.parent()).map(|p| p.to_path_buf()).unwrap_or_else(|| target.clone());
    
    let mut companions = BTreeMap::new();
    for name in ["javac", "jar"] {
        if let Some(path) = find_file(&home, name, 3) { companions.insert(name.to_string(), path); }
    }
    Ok((ProvisionedRuntime { runtime: RuntimeKind::Java, version, home, executable, companions, from_host: false, source: "Eclipse Adoptium Temurin JDK".to_string() }, format!("Fetched Temurin JDK {version} from Adoptium (SHA-256 verified).")))
}

// Below functions identical to Azaan's logic
fn choose_python(minimum: Option<Version>) -> Result<Version> {
    let default = Version::new(3, 12, 16);
    let wanted = minimum.unwrap_or(default);
    if wanted <= default { return Ok(default); }
    for (major, minor, patch) in PYTHON_BUILDS {
        let candidate = Version::new(*major, *minor, *patch);
        if candidate >= wanted { return Ok(candidate); }
    }
    Err(Error::Message(format!("no pinned build satisfies Python >= {wanted}")))
}

fn choose_node(minimum: Option<Version>, options: &ProvisionOptions) -> Result<Version> {
    let Some(minimum) = minimum else { return Ok(NODE_DEFAULT); };
    if minimum <= NODE_DEFAULT { return Ok(NODE_DEFAULT); }
    let cache = linuxorchestrator_core::paths::downloads_dir().join("node-index.json");
    if let Ok(text) = fetch_text_cached("https://nodejs.org/dist/index.json", &cache, options.offline) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(items) = value.as_array() {
                for item in items {
                    if let Some(version) = item["version"].as_str().and_then(Version::parse) {
                        if version >= minimum { return Ok(version); }
                    }
                }
            }
        }
    }
    if options.offline { return Err(Error::Download(format!("Node.js >= {minimum} is not cached."))); }
    Ok(minimum)
}

fn choose_java_feature(minimum: Option<Version>) -> u32 {
    let major = minimum.unwrap_or(Version::new(21, 0, 0)).major;
    if major <= 8 { 8 } else if major <= 11 { 11 } else if major <= 17 { 17 } else if major <= 21 { 21 } else { 25 }
}

fn ensure_extracted(target: &Path, archive: &Path, executable_name: &str) -> Result<PathBuf> {
    if target.exists() {
        if let Some(found) = find_file(target, executable_name, 5) { return Ok(found); }
        fsutil::remove_tree(target)?;
    }
    extract_archive(archive, target)?;
    find_file(target, executable_name, 6).ok_or_else(|| Error::Message(format!("archive did not contain {executable_name} under {}", target.display())))
}

fn find_file(root: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = WalkDir::new(root).follow_links(false).max_depth(max_depth).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file() && e.file_name().to_string_lossy() == name).map(|e| e.path().to_path_buf()).collect();
    matches.sort_by_key(|p| (p.components().count(), p.to_path_buf()));
    matches.into_iter().next()
}