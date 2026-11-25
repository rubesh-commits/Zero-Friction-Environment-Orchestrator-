use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use linuxorchestrator_core::{
    paths, Dependency, Entrypoint, Error, Language, ManifestInfo, ResolutionPlan, Result,
    RuntimeRecord, SandboxRecord, SourceKind, VersionReq,
};

/// A serialisable, human-editable copy of the analysis that produced an
/// environment. It deliberately mirrors `ResolutionPlan` rather than embedding
/// it so the TOML stays readable and commentable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPlan {
    #[serde(default)]
    pub languages: Vec<Language>,
    #[serde(default)]
    pub manifests: Vec<ManifestInfo>,
    #[serde(default)]
    pub doc_hints: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for SnapshotPlan {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            manifests: Vec::new(),
            doc_hints: Vec::new(),
            notes: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRequirement {
    pub runtime: linuxorchestrator_core::RuntimeKind,
    pub constraint: VersionReq,
    #[serde(default)]
    pub minimum: Option<linuxorchestrator_core::Version>,
    pub reason: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSpec {
    pub schema_version: u32,
    pub generated_at: String,
    pub tool: String,
    pub tool_version: String,
    pub sandbox_id: String,
    pub repo_name: String,
    pub source: String,
    pub source_kind: SourceKind,
    pub target: PathBuf,
    #[serde(default)]
    pub plan: SnapshotPlan,
    #[serde(default)]
    pub requirements: Vec<SnapshotRequirement>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub entrypoints: Vec<Entrypoint>,
    #[serde(default)]
    pub runtimes: Vec<RuntimeRecord>,
    #[serde(default)]
    pub launch: Option<linuxorchestrator_core::LaunchRecord>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl RestoreSpec {
    pub fn from_record(record: &SandboxRecord) -> Self {
        let requirements = record
            .plan
            .requirements
            .iter()
            .map(|requirement| SnapshotRequirement {
                runtime: requirement.runtime,
                constraint: requirement.constraint.clone(),
                minimum: requirement.minimum,
                reason: requirement.reason.clone(),
                source: requirement.source.clone(),
            })
            .collect();
        Self {
            schema_version: linuxorchestrator_core::SNAPSHOT_SCHEMA_VERSION,
            generated_at: paths::iso8601_now(),
            tool: linuxorchestrator_core::PRODUCT_NAME.to_string(),
            tool_version: linuxorchestrator_core::PRODUCT_VERSION.to_string(),
            sandbox_id: record.id.clone(),
            repo_name: record.repo_name.clone(),
            source: record.source.clone(),
            source_kind: record.source_kind,
            target: record.plan.target.clone(),
            plan: SnapshotPlan {
                languages: record.plan.languages.clone(),
                manifests: record.plan.manifests.clone(),
                doc_hints: record.plan.doc_hints.clone(),
                notes: record.plan.notes.clone(),
                warnings: record.plan.warnings.clone(),
            },
            requirements,
            dependencies: record.plan.dependencies.clone(),
            entrypoints: record.plan.entrypoints.clone(),
            runtimes: record.runtimes.clone(),
            launch: record.launch.clone(),
            environment: record.environment.clone(),
            notes: record.notes.clone(),
            warnings: record.warnings.clone(),
        }
    }

    pub fn to_plan(&self) -> ResolutionPlan {
        let mut plan = ResolutionPlan::new(self.target.clone(), self.repo_name.clone());
        plan.languages = self.plan.languages.clone();
        plan.manifests = self.plan.manifests.clone();
        plan.doc_hints = self.plan.doc_hints.clone();
        plan.warnings = self.plan.warnings.clone();
        plan.notes = self.plan.notes.clone();
        plan.requirements = self
            .requirements
            .iter()
            .map(|requirement| linuxorchestrator_core::Requirement {
                runtime: requirement.runtime,
                constraint: requirement.constraint.clone(),
                minimum: requirement.minimum,
                reason: requirement.reason.clone(),
                source: requirement.source.clone(),
            })
            .collect();
        plan.dependencies = self.dependencies.clone();
        plan.entrypoints = self.entrypoints.clone();
        plan
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version > linuxorchestrator_core::SNAPSHOT_SCHEMA_VERSION {
            return Err(Error::Message(format!(
                "{} uses restore schema {}, but this build understands up to schema {}. Upgrade LinuxOrchestrator.",
                self.repo_name,
                self.schema_version,
                linuxorchestrator_core::SNAPSHOT_SCHEMA_VERSION
            )));
        }
        if self.repo_name.trim().is_empty() {
            return Err(Error::Message(
                "restore file has an empty repo_name".to_string(),
            ));
        }
        if self.source.trim().is_empty() {
            return Err(Error::Message(
                "restore file has an empty source".to_string(),
            ));
        }
        Ok(())
    }
}

/// Suggested output path: `<current-dir>/<repo>.envspec.utf8`.
pub fn default_snapshot_path(repo_name: &str, cwd: &Path) -> PathBuf {
    paths::default_snapshot_file(repo_name, cwd)
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn array(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|value| quote(value)).collect();
    format!("[{}]", items.join(", "))
}

fn key(text: &str) -> String {
    quote(text)
}

fn write_list(lines: &mut Vec<String>, key_name: &str, values: &[String]) {
    lines.push(format!("{key_name} = {}", array(values)));
}

/// Write the mandatory restore artifact. The file is strict UTF-8 with no BOM
/// and includes comments explaining every injection/fetch decision.
pub fn write_snapshot(spec: &RestoreSpec, path: &Path) -> Result<()> {
    spec.validate()?;
    let mut lines: Vec<String> = Vec::new();
    lines.push("# LinuxOrchestrator restore specification".to_string());
    lines.push(
        "# Strict UTF-8, no BOM. Human-readable documentation and executable config.".to_string(),
    );
    lines.push(format!(
        "# Rebuild with: linuxorchestrator restore {}",
        quote(&path.display().to_string())
    ));
    lines.push(String::new());
    lines.push(format!("schema_version = {}", spec.schema_version));
    lines.push(format!("generated_at = {}", quote(&spec.generated_at)));
    lines.push(format!("tool = {}", quote(&spec.tool)));
    lines.push(format!("tool_version = {}", quote(&spec.tool_version)));
    lines.push(format!("sandbox_id = {}", quote(&spec.sandbox_id)));
    lines.push(format!("repo_name = {}", quote(&spec.repo_name)));
    lines.push(format!("source = {}", quote(&spec.source)));
    lines.push(format!(
        "source_kind = {}",
        quote(spec.source_kind.as_str())
    ));
    lines.push(format!(
        "target = {}",
        quote(&spec.target.display().to_string())
    ));
    write_list(&mut lines, "notes", &spec.notes);
    write_list(&mut lines, "warnings", &spec.warnings);
    lines.push(String::new());

    lines.push("[plan]".to_string());
    lines.push("# Languages and documentation signals that drove the resolution.".to_string());
    let languages: Vec<String> = spec
        .plan
        .languages
        .iter()
        .map(|language| language.as_str().to_string())
        .collect();
    write_list(&mut lines, "languages", &languages);
    write_list(&mut lines, "doc_hints", &spec.plan.doc_hints);
    write_list(&mut lines, "notes", &spec.plan.notes);
    write_list(&mut lines, "warnings", &spec.plan.warnings);
    for manifest in &spec.plan.manifests {
        lines.push("[[plan.manifests]]".to_string());
        lines.push(format!("path = {}", quote(&manifest.path)));
        lines.push(format!("kind = {}", quote(&manifest.kind)));
        lines.push(String::new());
    }
    lines.push(String::new());

    for requirement in &spec.requirements {
        lines.push("[[requirements]]".to_string());
        lines.push(format!(
            "runtime = {}  # {}",
            quote(requirement.runtime.as_str()),
            requirement.runtime.display_name()
        ));
        lines.push(format!(
            "constraint = {}",
            quote(&requirement.constraint.to_string())
        ));
        if let Some(minimum) = requirement.minimum {
            lines.push(format!("minimum = {}", quote(&minimum.to_string())));
        }
        lines.push(format!("reason = {}", quote(&requirement.reason)));
        lines.push(format!("source = {}", quote(&requirement.source)));
        lines.push(String::new());
    }

    for dependency in &spec.dependencies {
        lines.push("[[dependencies]]".to_string());
        lines.push(format!("name = {}", quote(&dependency.name)));
        if let Some(requirement) = &dependency.requirement {
            lines.push(format!("requirement = {}", quote(requirement)));
        }
        lines.push(format!(
            "kind = {}",
            quote(match dependency.kind {
                linuxorchestrator_core::DependencyKind::Runtime => "runtime",
                linuxorchestrator_core::DependencyKind::Dev => "dev",
                linuxorchestrator_core::DependencyKind::Build => "build",
                linuxorchestrator_core::DependencyKind::Optional => "optional",
            })
        ));
        lines.push(format!("source_file = {}", quote(&dependency.source_file)));
        lines.push(format!(
            "origin = {}",
            quote(match dependency.origin {
                linuxorchestrator_core::DependencyOrigin::Manifest => "manifest",
                linuxorchestrator_core::DependencyOrigin::Ast => "ast",
            })
        ));
        if let Some(language) = dependency.language {
            lines.push(format!("language = {}", quote(language.as_str())));
        }
        lines.push(String::new());
    }

    for entrypoint in &spec.entrypoints {
        lines.push("[[entrypoints]]".to_string());
        lines.push(format!("kind = {}", quote(entrypoint_kind(entrypoint))));
        if let Some(path) = &entrypoint.path {
            lines.push(format!("path = {}", quote(path)));
        }
        write_list(&mut lines, "args", &entrypoint.args);
        lines.push(format!("reason = {}", quote(&entrypoint.reason)));
        lines.push(String::new());
    }

    for runtime in &spec.runtimes {
        lines.push("[[runtimes]]".to_string());
        lines.push(format!("runtime = {}", quote(runtime.runtime.as_str())));
        lines.push(format!("version = {}", quote(&runtime.version.to_string())));
        lines.push(format!(
            "executable = {}",
            quote(&runtime.executable.display().to_string())
        ));
        lines.push(format!(
            "home = {}",
            quote(&runtime.home.display().to_string())
        ));
        lines.push(format!("from_host = {}", runtime.from_host));
        if runtime.from_host {
            lines.push("# Injected from the host; no download was needed.".to_string());
        } else {
            lines.push(
                "# Fetched from the official vendor endpoint and SHA-256 verified.".to_string(),
            );
        }
        lines.push(format!("source = {}", quote(&runtime.source)));
        if !runtime.companions.is_empty() {
            lines.push("[runtimes.companions]".to_string());
            for (name, value) in &runtime.companions {
                lines.push(format!(
                    "{} = {}",
                    key(name),
                    quote(&value.display().to_string())
                ));
            }
        }
        lines.push(String::new());
    }

    if let Some(launch) = &spec.launch {
        lines.push("[launch]".to_string());
        lines.push("# The exact command the sandbox used; restore reproduces it.".to_string());
        lines.push(format!("program = {}", quote(&launch.program)));
        write_list(&mut lines, "args", &launch.args);
        lines.push(format!(
            "cwd = {}",
            quote(&launch.cwd.display().to_string())
        ));
        lines.push(format!("display = {}", quote(&launch.display)));
        if let Some(entrypoint) = &launch.entrypoint {
            lines.push("[launch.entrypoint]".to_string());
            lines.push(format!("kind = {}", quote(entrypoint_kind(entrypoint))));
            if let Some(path) = &entrypoint.path {
                lines.push(format!("path = {}", quote(path)));
            }
            write_list(&mut lines, "args", &entrypoint.args);
            lines.push(format!("reason = {}", quote(&entrypoint.reason)));
        }
        lines.push(String::new());
    }

    lines.push("[environment]".to_string());
    lines.push("# Scoped environment. Host PATH is intentionally absent.".to_string());
    for (name, value) in &spec.environment {
        lines.push(format!("{} = {}", key(name), quote(value)));
    }
    lines.push(String::new());

    linuxorchestrator_core::fsutil::write_utf8(path, &lines.join("\n"))?;
    Ok(())
}

fn entrypoint_kind(entrypoint: &Entrypoint) -> &'static str {
    match entrypoint.kind {
        linuxorchestrator_core::EntrypointKind::PythonScript => "python_script",
        linuxorchestrator_core::EntrypointKind::PythonModule => "python_module",
        linuxorchestrator_core::EntrypointKind::NodeScript => "node_script",
        linuxorchestrator_core::EntrypointKind::NpmScript => "npm_script",
        linuxorchestrator_core::EntrypointKind::JavaMain => "java_main",
        linuxorchestrator_core::EntrypointKind::JavaJar => "java_jar",
        linuxorchestrator_core::EntrypointKind::Maven => "maven",
        linuxorchestrator_core::EntrypointKind::Gradle => "gradle",
        linuxorchestrator_core::EntrypointKind::RustBinary => "rust_binary",
        linuxorchestrator_core::EntrypointKind::GoProgram => "go_program",
        linuxorchestrator_core::EntrypointKind::Shell => "shell",
        linuxorchestrator_core::EntrypointKind::Command => "command",
    }
}

/// Read and validate a restore file. Invalid UTF-8 is rejected rather than
/// silently replaced, because the file is meant to round-trip exactly.
pub fn read_snapshot(path: &Path) -> Result<RestoreSpec> {
    let text = linuxorchestrator_core::fsutil::read_utf8_strict(path)?;
    let spec: RestoreSpec = toml::from_str(&text).map_err(|err| {
        Error::Message(format!(
            "could not parse restore file {}: {err}",
            path.display()
        ))
    })?;
    spec.validate()?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxorchestrator_core::{
        DependencyKind, DependencyOrigin, EntrypointKind, ExecutionProfile, LifecycleState,
        RuntimeKind, Version,
    };

    fn sample_record() -> SandboxRecord {
        let mut plan = ResolutionPlan::new(
            PathBuf::from("/work/café-repo"),
            "café-repo".to_string(),
        );
        plan.languages = vec![Language::Python];
        plan.requirements.push(linuxorchestrator_core::Requirement {
            runtime: RuntimeKind::Python,
            constraint: VersionReq::parse(">=3.10").unwrap(),
            minimum: Some(Version::new(3, 10, 0)),
            reason: "walrus/match syntax".to_string(),
            source: "app.py".to_string(),
        });
        plan.dependencies.push(Dependency {
            name: "requests".to_string(),
            requirement: Some(">=2.31".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "requirements.txt".to_string(),
            origin: DependencyOrigin::Manifest,
            language: Some(Language::Python),
        });
        plan.entrypoints.push(Entrypoint::new(
            EntrypointKind::PythonScript,
            Some("app.py".to_string()),
            "has a __main__ guard",
        ));
        plan.notes.push("note with → arrow".to_string());
        SandboxRecord {
            schema_version: 1,
            id: "café-repo-1".to_string(),
            repo_name: "café-repo".to_string(),
            source: "/work/café-repo".to_string(),
            source_kind: SourceKind::Local,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            root: PathBuf::from("/sandbox"),
            workspace: PathBuf::from("/sandbox/workspace"),
            plan,
            runtimes: vec![RuntimeRecord {
                runtime: RuntimeKind::Python,
                version: Version::new(3, 12, 16),
                executable: PathBuf::from("/usr/bin/python3"),
                home: PathBuf::from("/usr"),
                from_host: true,
                source: "host (PATH)".to_string(),
                companions: Default::default(),
            }],
            environment: [("PATH".to_string(), "/usr/bin".to_string())]
                .into_iter()
                .collect(),
            launch: Some(linuxorchestrator_core::LaunchRecord {
                program: "/usr/bin/python3".to_string(),
                args: vec!["app.py".to_string()],
                cwd: PathBuf::from("/sandbox/workspace"),
                display: "/usr/bin/python3 app.py".to_string(),
                entrypoint: None,
            }),
            profile: ExecutionProfile::default(),
            lifecycle: LifecycleState::Prepared,
            pid: None,
            notes: vec!["injected host Python".to_string()],
            warnings: Vec::new(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("lo-snapshot-{name}-{stamp}"))
    }

    #[test]
    fn writes_strict_utf8_with_comments() {
        let dir = scratch("utf8");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("repo.envspec.utf8");
        let spec = RestoreSpec::from_record(&sample_record());
        write_snapshot(&spec, &file).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("# LinuxOrchestrator restore specification"));
        assert!(text.contains("Injected from the host"));
        assert!(text.contains("café-repo"));
        linuxorchestrator_core::fsutil::remove_tree(&dir).unwrap();
    }

    #[test]
    fn roundtrips_all_decision_data() {
        let dir = scratch("roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("repo.envspec.utf8");
        let spec = RestoreSpec::from_record(&sample_record());
        write_snapshot(&spec, &file).unwrap();
        let loaded = read_snapshot(&file).unwrap();
        assert_eq!(loaded.repo_name, spec.repo_name);
        assert_eq!(loaded.requirements, spec.requirements);
        assert_eq!(loaded.runtimes, spec.runtimes);
        assert_eq!(loaded.launch, spec.launch);
        assert_eq!(loaded.environment, spec.environment);
        let plan = loaded.to_plan();
        assert_eq!(plan.languages, vec![Language::Python]);
        assert_eq!(plan.entrypoints.len(), 1);
        assert_eq!(plan.requirements[0].minimum, Some(Version::new(3, 10, 0)));
        linuxorchestrator_core::fsutil::remove_tree(&dir).unwrap();
    }

    #[test]
    fn legacy_dependencies_without_language_still_parse() {
        let dir = scratch("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("legacy.envspec.utf8");
        let mut spec = RestoreSpec::from_record(&sample_record());
        spec.dependencies = Vec::new();
        write_snapshot(&spec, &file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let legacy = format!(
            "{text}\n[[dependencies]]\nname = \"requests\"\nkind = \"runtime\"\nsource_file = \"requirements.txt\"\norigin = \"manifest\"\n"
        );
        std::fs::write(&file, legacy).unwrap();

        let loaded = read_snapshot(&file).unwrap();
        assert_eq!(loaded.dependencies.len(), 1);
        assert_eq!(loaded.dependencies[0].name, "requests");
        assert_eq!(loaded.dependencies[0].language, None);
        linuxorchestrator_core::fsutil::remove_tree(&dir).unwrap();
    }

    #[test]
    fn rejects_non_utf8_files() {
        let dir = scratch("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bad.envspec.utf8");
        std::fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_snapshot(&file).is_err());
        linuxorchestrator_core::fsutil::remove_tree(&dir).unwrap();
    }

    #[test]
    fn rejects_future_schema() {
        let dir = scratch("future");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("future.envspec.utf8");
        let mut spec = RestoreSpec::from_record(&sample_record());
        spec.schema_version = 99;
        write_snapshot(&spec, &file).unwrap_err();
        linuxorchestrator_core::fsutil::remove_tree(&dir).unwrap();
    }
}
