use crate::readme::ReadmeAnalysis;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use linuxorchestrator_core::{Entrypoint, EntrypointKind};

pub struct EntrypointContext<'a> {
    pub root: &'a Path,
    pub relative_files: &'a BTreeSet<String>,
    pub readme: &'a ReadmeAnalysis,
}

/// Combine manifest-declared entrypoints with deterministic file discovery.
pub fn discover(
    manifest_entries: Vec<Entrypoint>,
    context: &EntrypointContext<'_>,
) -> Vec<Entrypoint> {
    let mut candidates = manifest_entries;

    for path in [
        "main.py",
        "app.py",
        "run.py",
        "manage.py",
        "server.py",
        "bot.py",
        "src/main.py",
        "src/app.py",
        "src/run.py",
    ] {
        if context.relative_files.contains(path) {
            candidates.push(Entrypoint::new(
                EntrypointKind::PythonScript,
                Some(path.to_string()),
                "conventional Python entry file",
            ));
        }
    }

    let main_guard = Regex::new(r#"(?m)^\s*if\s+__name__\s*==\s*['"]__main__['"]"#).unwrap();
    let mut guarded = 0;
    for path in context
        .relative_files
        .iter()
        .filter(|path| path.ends_with(".py"))
    {
        if guarded >= 5 || !is_small_text(context.root, path, 512 * 1024) {
            continue;
        }
        let full = context.root.join(path);
        if let Ok(text) = std::fs::read_to_string(full) {
            if main_guard.is_match(&text) {
                candidates.push(Entrypoint::new(
                    EntrypointKind::PythonScript,
                    Some(path.clone()),
                    "contains an `if __name__ == \"__main__\"` guard",
                ));
                guarded += 1;
            }
        }
    }

    for path in [
        "index.js",
        "server.js",
        "app.js",
        "main.js",
        "src/index.js",
        "src/server.js",
        "src/main.js",
        "src/app.js",
    ] {
        if context.relative_files.contains(path) {
            candidates.push(Entrypoint::new(
                EntrypointKind::NodeScript,
                Some(path.to_string()),
                "conventional Node.js entry file",
            ));
        }
    }

    // Plain Java main classes.
    let main_re = Regex::new(
        r"(?m)\bpublic\s+static\s+void\s+main\s*\(\s*(?:final\s+)?String(?:\s*\[\s*\]\s*|\s*\.\.\.\s*)[A-Za-z_]\w*",
    )
    .unwrap();
    let package_re = Regex::new(r"(?m)^\s*package\s+([A-Za-z_][\w.]*)\s*;").unwrap();
    let class_re = Regex::new(
        r"(?m)\b(?:public\s+)?(?:final\s+|abstract\s+)?(?:class|record|enum)\s+([A-Za-z_]\w*)",
    )
    .unwrap();
    let mut java_mains = 0;
    for path in context
        .relative_files
        .iter()
        .filter(|path| path.ends_with(".java"))
    {
        if java_mains >= 8 || !is_small_text(context.root, path, 512 * 1024) {
            continue;
        }
        let full = context.root.join(path);
        let Ok(text) = std::fs::read_to_string(full) else {
            continue;
        };
        if !main_re.is_match(&text) {
            continue;
        }
        let package = package_re
            .captures(&text)
            .map(|captures| captures[1].to_string())
            .unwrap_or_default();
        let main_offset = main_re.find(&text).unwrap().start();
        // Pick the type declaration that encloses the first main method. A file
        // may declare helper records/classes before the real entrypoint.
        let class = class_re
            .captures_iter(&text)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                (whole.start() <= main_offset).then(|| captures[1].to_string())
            })
            .last()
            .or_else(|| {
                Path::new(path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
            })
            .unwrap_or_default();
        if class.is_empty() {
            continue;
        }
        let main_class = if package.is_empty() {
            class
        } else {
            format!("{package}.{class}")
        };
        candidates.push(Entrypoint::new(
            EntrypointKind::JavaMain,
            Some(main_class),
            format!("{path} declares a main method"),
        ));
        java_mains += 1;
    }

    if context.relative_files.contains("src/main.rs") {
        candidates.push(Entrypoint::new(
            EntrypointKind::RustBinary,
            Some(".".to_string()),
            "src/main.rs exists",
        ));
    }

    let go_main = Regex::new(r"(?m)^\s*func\s+main\s*\(\s*\)").unwrap();
    let go_package = Regex::new(r"(?m)^\s*package\s+main\b").unwrap();
    let mut go_found = false;
    for path in context
        .relative_files
        .iter()
        .filter(|path| path.ends_with(".go"))
    {
        if go_found || !is_small_text(context.root, path, 512 * 1024) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(context.root.join(path)) else {
            continue;
        };
        if go_package.is_match(&text) && go_main.is_match(&text) {
            candidates.push(Entrypoint::new(
                EntrypointKind::GoProgram,
                Some(".".to_string()),
                "Go package main with func main",
            ));
            go_found = true;
        }
    }

    for hint in &context.readme.hints {
        if let Some(entry) = entrypoint_from_hint(hint, context.relative_files) {
            candidates.push(entry);
        }
    }

    dedupe(candidates)
}

fn entrypoint_from_hint(hint: &str, files: &BTreeSet<String>) -> Option<Entrypoint> {
    let tokens: Vec<&str> = hint.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let command = tokens[0].to_ascii_lowercase();
    let reason = format!("README hint: {hint}");
    match command.as_str() {
        "python" | "python3" | "py" => {
            let script = tokens
                .iter()
                .skip(1)
                .find(|token| !token.starts_with('-'))?;
            let script = script.trim_matches('"').replace('\\', "/");
            let script = script.trim_start_matches("./");
            if files.contains(script) {
                let mut entry = Entrypoint::new(
                    EntrypointKind::PythonScript,
                    Some(script.to_string()),
                    reason,
                );
                entry.args = tokens
                    .iter()
                    .skip_while(|token| !token.ends_with(script))
                    .skip(1)
                    .map(|token| token.to_string())
                    .collect();
                Some(entry)
            } else {
                None
            }
        }
        "node" => {
            let script = tokens
                .iter()
                .skip(1)
                .find(|token| !token.starts_with('-'))?;
            let script = script.trim_matches('"').replace('\\', "/");
            let script = script.trim_start_matches("./");
            if files.contains(script) {
                Some(Entrypoint::new(
                    EntrypointKind::NodeScript,
                    Some(script.to_string()),
                    reason,
                ))
            } else {
                None
            }
        }
        "npm" | "pnpm" | "yarn" => {
            let script = if tokens.len() >= 3 && tokens[1] == "run" {
                tokens[2]
            } else if tokens.len() >= 2 {
                tokens[1]
            } else {
                return None;
            };
            if matches!(script, "start" | "dev" | "serve") {
                Some(
                    Entrypoint::new(EntrypointKind::NpmScript, Some(script.to_string()), reason)
                        .with_args(&["run", script]),
                )
            } else {
                None
            }
        }
        "java" if tokens.get(1) == Some(&"-jar") => {
            let jar = tokens.get(2)?.trim_matches('"').replace('\\', "/");
            let jar = jar.trim_start_matches("./");
            files
                .contains(jar)
                .then(|| Entrypoint::new(EntrypointKind::JavaJar, Some(jar.to_string()), reason))
        }
        "mvn" | "mvnw" if hint.contains("spring-boot:run") => Some(
            Entrypoint::new(
                EntrypointKind::Maven,
                Some(if command == "mvnw" { "mvnw" } else { "mvn" }.to_string()),
                reason,
            )
            .with_args(&["spring-boot:run"]),
        ),
        "gradle" | "gradlew" if hint.contains("bootRun") => Some(
            Entrypoint::new(
                EntrypointKind::Gradle,
                Some(
                    if command == "gradlew" {
                        "gradlew"
                    } else {
                        "gradle"
                    }
                    .to_string(),
                ),
                reason,
            )
            .with_args(&["bootRun"]),
        ),
        "cargo" if tokens.get(1) == Some(&"run") => Some(Entrypoint::new(
            EntrypointKind::RustBinary,
            Some(".".to_string()),
            reason,
        )),
        "go" if tokens.get(1) == Some(&"run") => Some(Entrypoint::new(
            EntrypointKind::GoProgram,
            Some(".".to_string()),
            reason,
        )),
        _ => None,
    }
}

fn is_small_text(root: &Path, relative: &str, limit: u64) -> bool {
    std::fs::metadata(root.join(PathBuf::from(relative)))
        .map(|metadata| metadata.len() <= limit)
        .unwrap_or(false)
}

fn dedupe(entries: Vec<Entrypoint>) -> Vec<Entrypoint> {
    let mut result: Vec<Entrypoint> = Vec::new();
    for entry in entries {
        let duplicate = result.iter().any(|existing| {
            existing.kind == entry.kind
                && existing.path == entry.path
                && existing.args == entry.args
        });
        if !duplicate {
            result.push(entry);
        }
        if result.len() >= 16 {
            break;
        }
    }
    result
}