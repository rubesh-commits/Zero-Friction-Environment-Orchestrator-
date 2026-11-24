use crate::download::{download_verified, extract_archive, fetch_text_cached, HashExpectation};
use crate::{DownloadRecord, ProvisionOptions, ProvisionedRuntime};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use winorchestrator_core::fsutil;
use winorchestrator_core::{Error, Result, RuntimeKind, Version};

/// Pinned python-build-standalone release. Archives are verified against the
/// release's own SHA256SUMS file at runtime.
const PBS_TAG: &str = "20261123";
const PBS_BASE: &str = "https://github.com/astral-sh/python-build-standalone/releases/download";

const PYTHON_BUILDS: &[(u32, u32, u32)] = &[(3, 11, 18), (3, 12, 16), (3, 13, 18), (3, 14, 10)];

const NODE_DEFAULT: Version = Version::new(24, 22, 3);

pub fn fetch_python(
    minimum: Option<Version>,
    install_root: &Path,
    downloads: &mut Vec<DownloadRecord>,
    options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let version = choose_python(minimum)?;
    let asset = format!("cpython-{version}+{PBS_TAG}-x86_64-pc-windows-msvc-install_only.tar.gz");
    let url = format!("{PBS_BASE}/{PBS_TAG}/{asset}");
    let sums_url = format!("{PBS_BASE}/{PBS_TAG}/SHA256SUMS");
    let downloaded = download_verified(
        &url,
        &asset,
        HashExpectation::SumsUrl {
            url: &sums_url,
            filename: &asset,
        },
        &winorchestrator_core::paths::downloads_dir(),
        options,
    )?;
    downloads.push(DownloadRecord {
        runtime: RuntimeKind::Python,
        version: version.to_string(),
        file: downloaded.path.clone(),
        bytes: downloaded.bytes,
        sha256: downloaded.sha256.clone(),
        source: url.clone(),
    });
    let target = install_root.join(format!("python-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "python.exe")?;
    let home = executable
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| target.clone());
    let mut companions = BTreeMap::new();
    for name in ["pip.exe", "pip3.exe", "pythonw.exe"] {
        if let Some(path) = find_file(&home, name, 3) {
            companions.insert(name.to_string(), path);
        }
    }
    let note = format!(
        "Fetched Python {version} from python-build-standalone (SHA-256 verified); pip and venv are included."
    );
    Ok((
        ProvisionedRuntime {
            runtime: RuntimeKind::Python,
            version,
            home,
            executable,
            companions,
            from_host: false,
            source: format!("python-build-standalone {PBS_TAG}"),
        },
        note,
    ))
}

pub fn fetch_node(
    minimum: Option<Version>,
    install_root: &Path,
    downloads: &mut Vec<DownloadRecord>,
    options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let version = choose_node(minimum, options)?;
    let asset = format!("node-v{version}-win-x64.zip");
    let url = format!("https://nodejs.org/dist/v{version}/{asset}");
    let sums_url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    let downloaded = download_verified(
        &url,
        &asset,
        HashExpectation::SumsUrl {
            url: &sums_url,
            filename: &asset,
        },
        &winorchestrator_core::paths::downloads_dir(),
        options,
    )?;
    downloads.push(DownloadRecord {
        runtime: RuntimeKind::Node,
        version: version.to_string(),
        file: downloaded.path.clone(),
        bytes: downloaded.bytes,
        sha256: downloaded.sha256.clone(),
        source: url.clone(),
    });
    let target = install_root.join(format!("node-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "node.exe")?;
    let home = executable
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| target.clone());
    let mut companions = BTreeMap::new();
    for name in ["npm.cmd", "npx.cmd", "corepack.cmd"] {
        if let Some(path) = find_file(&home, name, 2) {
            companions.insert(name.to_string(), path);
        }
    }
    let note = format!(
        "Fetched Node.js {version} from nodejs.org (SHA-256 verified); npm and npx are included."
    );
    Ok((
        ProvisionedRuntime {
            runtime: RuntimeKind::Node,
            version,
            home,
            executable,
            companions,
            from_host: false,
            source: "nodejs.org dist".to_string(),
        },
        note,
    ))
}

pub fn fetch_java(
    minimum: Option<Version>,
    install_root: &Path,
    downloads: &mut Vec<DownloadRecord>,
    options: &ProvisionOptions,
) -> Result<(ProvisionedRuntime, String)> {
    let feature = choose_java_feature(minimum);
    let api = format!(
        "https://api.adoptium.net/v3/assets/latest/{feature}/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse&page=0&page_size=1"
    );
    let cache =
        winorchestrator_core::paths::downloads_dir().join(format!("adoptium-{feature}.json"));
    let text = fetch_text_cached(&api, &cache, options.offline)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let item = value.get(0).ok_or_else(|| {
        Error::Download(format!("Adoptium returned no JDK build for Java {feature}"))
    })?;
    let package = &item["binary"]["package"];
    let link = package["link"]
        .as_str()
        .ok_or_else(|| Error::Download("Adoptium response had no package link".to_string()))?;
    let checksum = package["checksum"]
        .as_str()
        .ok_or_else(|| Error::Download("Adoptium response had no checksum".to_string()))?;
    let filename = package["name"]
        .as_str()
        .map(|name| name.to_string())
        .unwrap_or_else(|| format!("temurin-{feature}-jdk-win-x64.zip"));
    let version = item["version"]["semver"]
        .as_str()
        .and_then(Version::parse)
        .unwrap_or_else(|| Version::new(feature, 0, 0));

    let downloaded = download_verified(
        link,
        &filename,
        HashExpectation::Inline(checksum),
        &winorchestrator_core::paths::downloads_dir(),
        options,
    )?;
    downloads.push(DownloadRecord {
        runtime: RuntimeKind::Java,
        version: version.to_string(),
        file: downloaded.path.clone(),
        bytes: downloaded.bytes,
        sha256: downloaded.sha256.clone(),
        source: link.to_string(),
    });
    let target = install_root.join(format!("java-{version}"));
    let executable = ensure_extracted(&target, &downloaded.path, "java.exe")?;
    let home = executable
        .parent()
        .and_then(|bin| bin.parent())
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| target.clone());
    let mut companions = BTreeMap::new();
    for name in ["javac.exe", "jar.exe", "javaw.exe"] {
        if let Some(path) = find_file(&home, name, 3) {
            companions.insert(name.to_string(), path);
        }
    }
    let note = format!(
        "Fetched Temurin JDK {version} from Adoptium (SHA-256 verified); javac is included."
    );
    Ok((
        ProvisionedRuntime {
            runtime: RuntimeKind::Java,
            version,
            home,
            executable,
            companions,
            from_host: false,
            source: "Eclipse Adoptium Temurin JDK".to_string(),
        },
        note,
    ))
}

fn choose_python(minimum: Option<Version>) -> Result<Version> {
    let default = Version::new(3, 12, 16);
    let wanted = minimum.unwrap_or(default);
    if wanted <= default {
        return Ok(default);
    }
    for (major, minor, patch) in PYTHON_BUILDS {
        let candidate = Version::new(*major, *minor, *patch);
        if candidate >= wanted {
            return Ok(candidate);
        }
    }
    Err(Error::Message(format!(
        "no pinned python-build-standalone build satisfies Python >= {wanted}; \
         install a newer Python on the host or update the pinned build table"
    )))
}

fn choose_node(minimum: Option<Version>, options: &ProvisionOptions) -> Result<Version> {
    let Some(minimum) = minimum else {
        return Ok(NODE_DEFAULT);
    };
    if minimum <= NODE_DEFAULT {
        return Ok(NODE_DEFAULT);
    }
    // Newer requirements need the live dist index. The chosen version is still
    // verified against that version's own SHASUMS256.txt.
    let cache = winorchestrator_core::paths::downloads_dir().join("node-index.json");
    if let Ok(text) = fetch_text_cached(
        "https://nodejs.org/dist/index.json",
        &cache,
        options.offline,
    ) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(items) = value.as_array() {
                for item in items {
                    if let Some(version) = item["version"].as_str().and_then(Version::parse) {
                        if version >= minimum {
                            return Ok(version);
                        }
                    }
                }
            }
        }
    }
    if options.offline {
        return Err(Error::Download(format!(
            "Node.js >= {minimum} is not cached and the host only knows Node {NODE_DEFAULT}"
        )));
    }
    Ok(minimum)
}

fn choose_java_feature(minimum: Option<Version>) -> u32 {
    let major = minimum.unwrap_or(Version::new(21, 0, 0)).major;
    if major <= 8 {
        8
    } else if major <= 11 {
        11
    } else if major <= 17 {
        17
    } else if major <= 21 {
        21
    } else {
        25
    }
}

fn ensure_extracted(target: &Path, archive: &Path, executable_name: &str) -> Result<PathBuf> {
    if target.exists() {
        if let Some(found) = find_file(target, executable_name, 5) {
            return Ok(found);
        }
        fsutil::remove_tree(target)?;
    }
    extract_archive(archive, target)?;
    find_file(target, executable_name, 6).ok_or_else(|| {
        Error::Message(format!(
            "the downloaded archive did not contain {executable_name} under {}",
            target.display()
        ))
    })
}

fn find_file(root: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(name)
        })
        .map(|entry| entry.path().to_path_buf())
        .collect();
    matches.sort_by_key(|path| (path.components().count(), path.to_path_buf()));
    matches.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_choice_satisfies_minimum() {
        assert_eq!(choose_python(None).unwrap(), Version::new(3, 12, 16));
        assert_eq!(
            choose_python(Some(Version::new(3, 8, 0))).unwrap(),
            Version::new(3, 12, 16)
        );
        assert_eq!(
            choose_python(Some(Version::new(3, 13, 0))).unwrap(),
            Version::new(3, 13, 18)
        );
        assert!(choose_python(Some(Version::new(3, 99, 0))).is_err());
    }

    #[test]
    fn java_choice_uses_supported_lts() {
        assert_eq!(choose_java_feature(None), 21);
        assert_eq!(choose_java_feature(Some(Version::new(8, 0, 0))), 8);
        assert_eq!(choose_java_feature(Some(Version::new(16, 0, 0))), 17);
        assert_eq!(choose_java_feature(Some(Version::new(22, 0, 0))), 25);
    }

    #[test]
    fn node_choice_defaults_without_network() {
        assert_eq!(
            choose_node(None, &ProvisionOptions::offline()).unwrap(),
            NODE_DEFAULT
        );
        assert_eq!(
            choose_node(Some(Version::new(18, 0, 0)), &ProvisionOptions::offline()).unwrap(),
            NODE_DEFAULT
        );
    }
}
