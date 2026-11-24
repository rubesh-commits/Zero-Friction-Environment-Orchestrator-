//! Deterministic repository analysis.
//!
//! Inputs are README/manifests, source code parsed with tree-sitter, and an
//! embedded compatibility table. There is deliberately no network call and no
//! LLM anywhere in this crate.

pub mod ast;
pub mod entrypoints;
pub mod manifests;
pub mod readme;
pub mod rules;

use entrypoints::EntrypointContext;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use winorchestrator_core::{
    Dependency, DependencyKind, DependencyOrigin, Entrypoint, Language, ManifestInfo, Requirement,
    ResolutionPlan, RuntimeKind, Version,
};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "env",
    "node_modules",
    "bower_components",
    "target",
    "dist",
    "build",
    "out",
    ".gradle",
    ".idea",
    ".vs",
    ".vscode",
    ".github",
    "vendor",
    "Pods",
    ".next",
    ".nuxt",
    ".cache",
    "coverage",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".nox",
];

#[derive(Clone, Debug)]
pub struct AnalyzeOptions {
    pub max_source_files: usize,
    pub max_ast_bytes: u64,
    pub timeout_seconds: u64,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            max_source_files: 3000,
            max_ast_bytes: 2 * 1024 * 1024,
            timeout_seconds: 120,
        }
    }
}

#[derive(Clone, Debug)]
struct SourceFile {
    absolute: PathBuf,
    relative: String,
    language: Option<Language>,
    size: u64,
}

/// Analyze a local repository directory (or a single file) without modifying
/// it. Remote acquisition is handled by the `git-acquire` crate.
pub fn analyze(target: &Path, repo_name: &str) -> winorchestrator_core::Result<ResolutionPlan> {
    analyze_with(target, repo_name, &AnalyzeOptions::default())
}

pub fn analyze_with(
    target: &Path,
    repo_name: &str,
    options: &AnalyzeOptions,
) -> winorchestrator_core::Result<ResolutionPlan> {
    let target = if target.exists() {
        std::path::absolute(target).unwrap_or_else(|_| target.to_path_buf())
    } else {
        return Err(winorchestrator_core::Error::Message(format!(
            "target does not exist: {}",
            target.display()
        )));
    };
    let root = if target.is_file() {
        target.parent().unwrap_or(&target).to_path_buf()
    } else {
        target.clone()
    };
    let mut plan = ResolutionPlan::new(target.clone(), repo_name.to_string());
    plan.notes.push(
        "Analysis is deterministic: manifest parsers + tree-sitter ASTs + embedded rules. No AI/LLM call is made."
            .to_string(),
    );
    plan.notes.push(
        "The original directory is treated as read-only; execution happens in a staged sandbox copy."
            .to_string(),
    );

    let (files, collection_warnings) = collect_files(&root, options);
    plan.warnings.extend(collection_warnings);

    let mut relative_files = BTreeSet::new();
    let mut languages = BTreeSet::new();
    for file in &files {
        relative_files.insert(file.relative.clone());
        if let Some(language) = file.language {
            languages.insert(language);
        }
    }

    // Parse manifests first so AST imports can be compared with declarations.
    let mut manifest_files: Vec<&SourceFile> = files
        .iter()
        .filter(|file| {
            Path::new(&file.relative)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(manifests::manifest_kind)
                .is_some()
        })
        .collect();
    manifest_files.sort_by_key(|file| {
        (
            file.relative.matches('/').count(),
            file.relative.to_ascii_lowercase(),
        )
    });

    let mut requirements: Vec<Requirement> = Vec::new();
    let mut dependencies: Vec<Dependency> = Vec::new();
    let mut manifest_entries: Vec<Entrypoint> = Vec::new();
    for file in manifest_files.iter().take(40) {
        let filename = Path::new(&file.relative)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let Some(kind) = manifests::manifest_kind(filename) else {
            continue;
        };
        plan.manifests.push(ManifestInfo {
            path: file.relative.clone(),
            kind: kind.to_string(),
        });
        let bundle = manifests::parse_manifest(kind, &file.absolute, &root, &file.relative);
        languages.extend(bundle.languages);
        requirements.extend(bundle.requirements);
        dependencies.extend(bundle.dependencies);
        manifest_entries.extend(bundle.entrypoints);
        plan.notes.extend(bundle.notes);
        plan.warnings.extend(bundle.warnings);
    }
    if plan.manifests.is_empty() {
        plan.warnings.push(
            "No known manifest was found. Entrypoints and dependencies are inferred from source and README."
                .to_string(),
        );
    }

    // README hints are documentation, not trusted commands.
    let readme = readme::analyze_readme(&root);
    if let Some(path) = &readme.path {
        plan.notes.push(format!(
            "Read {path} for setup/run hints ({} candidates).",
            readme.hints.len()
        ));
    }
    plan.doc_hints = readme.hints.clone();

    // Deterministic AST inspection.
    let mut source_files: Vec<&SourceFile> = files
        .iter()
        .filter(|file| {
            file.language.is_some()
                && file.size <= options.max_ast_bytes
                && !file.relative.to_ascii_lowercase().contains(".min.")
        })
        .collect();
    source_files.sort_by(|a, b| a.relative.cmp(&b.relative));
    let source_total = source_files.len();
    source_files.truncate(options.max_source_files);
    if source_total > source_files.len() {
        plan.warnings.push(format!(
            "Parsed the first {} of {source_total} source files to keep analysis bounded.",
            source_files.len()
        ));
    }

    let declared: BTreeSet<String> = dependencies
        .iter()
        .map(|dep| normalize_name(&dep.name))
        .collect();
    let mut undeclared_seen: BTreeSet<String> = BTreeSet::new();
    let mut rule_hits = Vec::new();
    let mut parse_warnings = 0usize;
    for file in &source_files {
        let Some(language) = file.language else {
            continue;
        };
        let report = ast::analyze_source(&file.absolute, &file.relative, language);
        if !report.warnings.is_empty() {
            if parse_warnings < 10 {
                plan.warnings.extend(report.warnings);
            }
            parse_warnings += 1;
        }
        rule_hits.extend(report.hits);
        if matches!(
            language,
            Language::Python | Language::JavaScript | Language::TypeScript
        ) {
            for module in report.imports {
                if normalize_name(&module).is_empty() {
                    continue;
                }
                if !declared.contains(&normalize_name(&module))
                    && undeclared_seen.insert(normalize_name(&module))
                {
                    dependencies.push(Dependency {
                        name: module.clone(),
                        requirement: None,
                        kind: DependencyKind::Runtime,
                        source_file: file.relative.clone(),
                        origin: DependencyOrigin::Ast,
                    });
                    if undeclared_seen.len() <= 20 {
                        plan.warnings.push(format!(
                            "`{module}` is imported in {} but is not declared in a parsed manifest; it will need installation.",
                            file.relative
                        ));
                    }
                }
            }
        }
    }

    // Add baselines for every detected language. These are deliberately
    // conservative; AST rules and manifests raise them when needed.
    for language in &languages {
        if let Some(runtime) = language.runtime() {
            let (constraint, why) = match runtime {
                RuntimeKind::Python => (">=3.8", "Python 3.8 baseline"),
                RuntimeKind::Node => (">=14", "Node.js 14 baseline"),
                RuntimeKind::Java => (">=8", "Java 8 baseline"),
                RuntimeKind::Rust => (">=1.56", "Rust 2021 edition baseline"),
                RuntimeKind::Go => (">=1.18", "Go 1.18 baseline"),
            };
            if let Some(req) = Requirement::new(runtime, constraint, why, "analyzer baseline") {
                requirements.push(req);
            }
        }
    }

    // Fold AST rules into runtime requirements and human-readable warnings.
    rule_hits.sort_by(|a, b| {
        a.language
            .cmp(&b.language)
            .then(a.minimum.cmp(&b.minimum))
            .then(a.construct.cmp(&b.construct))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    let mut seen_rules: BTreeSet<String> = BTreeSet::new();
    let mut warning_budget = 18usize;
    for hit in rule_hits {
        let Some(runtime) = hit.language.runtime() else {
            continue;
        };
        let key = format!("{}|{}", runtime.as_str(), hit.construct);
        if !seen_rules.insert(key) {
            continue;
        }
        let source = if hit.line > 0 {
            format!("{}:{}", hit.file, hit.line)
        } else {
            hit.file.clone()
        };
        if let Some(req) = Requirement::new(
            runtime,
            &format!(">={}", hit.minimum),
            &hit.explanation,
            &source,
        ) {
            requirements.push(req);
        }
        if warning_budget > 0 {
            let snippet = if hit.snippet.is_empty() {
                String::new()
            } else {
                format!("  [{}]", hit.snippet)
            };
            plan.warnings.push(format!(
                "{}: `{}` requires >= {} ({source}).{snippet}",
                hit.language.display_name(),
                hit.construct,
                hit.minimum
            ));
            warning_budget -= 1;
        }
    }

    // Deduplicate requirements and sort by runtime, newest requirement first.
    requirements = dedupe_requirements(requirements);
    requirements.sort_by(|a, b| {
        a.runtime
            .cmp(&b.runtime)
            .then(b.minimum.cmp(&a.minimum))
            .then(a.reason.cmp(&b.reason))
    });

    let entrypoints = entrypoints::discover(
        manifest_entries,
        &EntrypointContext {
            root: &root,
            relative_files: &relative_files,
            readme: &readme,
        },
    );
    if entrypoints.is_empty() {
        plan.warnings.push(
            "No automatic entrypoint was detected. Use `--entry <file>` or `--command` when running."
                .to_string(),
        );
    }

    plan.languages = languages.into_iter().collect();
    plan.requirements = requirements;
    plan.dependencies = merge_dependencies(dependencies);
    plan.entrypoints = entrypoints;

    plan.notes.push(format!(
        "Scanned {} files ({} source files parsed).",
        files.len(),
        source_files.len()
    ));

    Ok(plan)
}

fn collect_files(root: &Path, options: &AnalyzeOptions) -> (Vec<SourceFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            !SKIP_DIRS.iter().any(|skip| {
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
                if warnings.len() < 10 {
                    warnings.push(format!("walk warning: {err}"));
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = match entry.path().strip_prefix(root) {
            Ok(relative) => relative.display().to_string().replace('\\', "/"),
            Err(_) => continue,
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let language = entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(Language::from_extension);
        files.push(SourceFile {
            absolute: entry.path().to_path_buf(),
            relative,
            language,
            size,
        });
        if files.len() > 50_000 {
            warnings
                .push("Repository has more than 50,000 files; analysis was truncated.".to_string());
            break;
        }
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    let _ = options;
    (files, warnings)
}

fn normalize_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .replace('/', "-")
}

fn dedupe_requirements(requirements: Vec<Requirement>) -> Vec<Requirement> {
    let mut map: BTreeMap<(String, String, String), Requirement> = BTreeMap::new();
    for req in requirements {
        let key = (
            format!("{:?}", req.runtime),
            req.reason.clone(),
            req.source.clone(),
        );
        map.entry(key).or_insert(req);
    }
    map.into_values().collect()
}

fn merge_dependencies(dependencies: Vec<Dependency>) -> Vec<Dependency> {
    let mut merged: Vec<Dependency> = Vec::new();
    for dep in dependencies {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.name.eq_ignore_ascii_case(&dep.name) && existing.kind == dep.kind
        }) {
            if existing.origin == DependencyOrigin::Ast && dep.origin == DependencyOrigin::Manifest
            {
                existing.origin = DependencyOrigin::Manifest;
                existing.source_file = dep.source_file.clone();
            }
            if existing.requirement.is_none() {
                existing.requirement = dep.requirement.clone();
            }
            continue;
        }
        merged.push(dep);
    }
    merged.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then(kind_rank(a.kind).cmp(&kind_rank(b.kind)))
    });
    merged
}

fn kind_rank(kind: DependencyKind) -> u8 {
    match kind {
        DependencyKind::Runtime => 0,
        DependencyKind::Dev => 1,
        DependencyKind::Build => 2,
        DependencyKind::Optional => 3,
    }
}

/// Convenience helper used by tests and the CLI's human-readable output.
pub fn requirement_label(runtime: RuntimeKind, minimum: Option<Version>) -> String {
    match minimum {
        Some(version) => format!("{} >= {version}", runtime.display_name()),
        None => runtime.display_name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("winorchestrator-analyzer-{name}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn python_project_with_undocumented_match_syntax() {
        let dir = fixture("python");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nrequires-python = \">=3.10\"\ndependencies = [\"requests>=2.31\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("src").join("main.py"),
            "import requests\n\ndef main(x):\n    match x:\n        case 1:\n            return requests.get('https://example.com')\n\nif __name__ == '__main__':\n    main(1)\n",
        )
        .unwrap();
        let plan = analyze(&dir, "demo").unwrap();
        assert!(plan.has_language(Language::Python));
        let req = plan.requirement_for(RuntimeKind::Python).unwrap();
        assert!(req.minimum.unwrap() >= Version::new(3, 10, 0));
        assert!(plan
            .dependencies
            .iter()
            .any(|d| d.name == "requests" && d.origin == DependencyOrigin::Manifest));
        assert!(plan
            .entrypoints
            .iter()
            .any(|e| e.path.as_deref() == Some("src/main.py")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn node_engines_and_optional_chaining_raise_the_floor() {
        let dir = fixture("node");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"demo","main":"index.js","engines":{"node":">=18"},"scripts":{"start":"node index.js"},"dependencies":{"express":"^4.0.0"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("index.js"),
            "const express = require('express');\nconst x = process.env?.PORT ?? 3000;\n",
        )
        .unwrap();
        let plan = analyze(&dir, "demo").unwrap();
        let req = plan.requirement_for(RuntimeKind::Node).unwrap();
        assert!(req.minimum.unwrap() >= Version::new(18, 0, 0));
        assert!(plan
            .entrypoints
            .iter()
            .any(|e| e.kind == winorchestrator_core::EntrypointKind::NpmScript));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn javascript_undeclared_import_is_reported() {
        let dir = fixture("undeclared");
        fs::write(dir.join("index.js"), "import axios from 'axios';\n").unwrap();
        let plan = analyze(&dir, "demo").unwrap();
        assert!(plan
            .dependencies
            .iter()
            .any(|d| d.name == "axios" && d.origin == DependencyOrigin::Ast));
        assert!(plan.warnings.iter().any(|w| w.contains("axios")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn readme_hint_becomes_entrypoint_only_when_file_exists() {
        let dir = fixture("readme");
        fs::write(dir.join("runme.py"), "print('hi')\n").unwrap();
        fs::write(
            dir.join("README.md"),
            "# Demo\n\n```bash\npython runme.py --flag\npython missing.py\n```\n",
        )
        .unwrap();
        let plan = analyze(&dir, "demo").unwrap();
        assert!(plan
            .entrypoints
            .iter()
            .any(|e| e.path.as_deref() == Some("runme.py")));
        assert!(!plan
            .entrypoints
            .iter()
            .any(|e| e.path.as_deref() == Some("missing.py")));
        fs::remove_dir_all(&dir).unwrap();
    }
}
