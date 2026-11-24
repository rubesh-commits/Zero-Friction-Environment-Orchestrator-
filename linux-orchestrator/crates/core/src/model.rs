use crate::version::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Source language detected in a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Python,
    JavaScript,
    TypeScript,
    Java,
    Rust,
    Go,
    Unknown,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Java => "java",
            Language::Rust => "rust",
            Language::Go => "go",
            Language::Unknown => "unknown",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Java => "Java",
            Language::Rust => "Rust",
            Language::Go => "Go",
            Language::Unknown => "Unknown",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "py" | "pyw" => Some(Language::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" | "tsx" => Some(Language::TypeScript),
            "java" => Some(Language::Java),
            "rs" => Some(Language::Rust),
            "go" => Some(Language::Go),
            _ => None,
        }
    }

    pub fn runtime(self) -> Option<RuntimeKind> {
        match self {
            Language::Python => Some(RuntimeKind::Python),
            Language::JavaScript | Language::TypeScript => Some(RuntimeKind::Node),
            Language::Java => Some(RuntimeKind::Java),
            Language::Rust => Some(RuntimeKind::Rust),
            Language::Go => Some(RuntimeKind::Go),
            Language::Unknown => None,
        }
    }
}

/// Runtime families the provisioner knows how to resolve on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Python,
    Node,
    Java,
    Rust,
    Go,
}

impl RuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeKind::Python => "python",
            RuntimeKind::Node => "node",
            RuntimeKind::Java => "java",
            RuntimeKind::Rust => "rust",
            RuntimeKind::Go => "go",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            RuntimeKind::Python => "Python",
            RuntimeKind::Node => "Node.js",
            RuntimeKind::Java => "Java",
            RuntimeKind::Rust => "Rust",
            RuntimeKind::Go => "Go",
        }
    }

    pub fn executable_names(self) -> &'static [&'static str] {
        match self {
            RuntimeKind::Python => &["python.exe", "python3.exe", "py.exe"],
            RuntimeKind::Node => &["node.exe"],
            RuntimeKind::Java => &["java.exe", "javac.exe"],
            RuntimeKind::Rust => &["rustc.exe", "cargo.exe"],
            RuntimeKind::Go => &["go.exe"],
        }
    }
}

/// A deterministic requirement derived from a manifest or parsed source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub runtime: RuntimeKind,
    pub constraint: VersionReq,
    pub minimum: Option<Version>,
    pub reason: String,
    pub source: String,
}

impl Requirement {
    pub fn new(
        runtime: RuntimeKind,
        constraint: &str,
        reason: impl Into<String>,
        source: impl Into<String>,
    ) -> Option<Self> {
        let constraint = VersionReq::parse(constraint)?;
        let minimum = constraint.minimum();
        Some(Self {
            runtime,
            constraint,
            minimum,
            reason: reason.into(),
            source: source.into(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Runtime,
    Dev,
    Build,
    Optional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyOrigin {
    Manifest,
    Ast,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub requirement: Option<String>,
    pub kind: DependencyKind,
    pub source_file: String,
    pub origin: DependencyOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntrypointKind {
    PythonScript,
    PythonModule,
    NodeScript,
    NpmScript,
    JavaMain,
    JavaJar,
    Maven,
    Gradle,
    RustBinary,
    GoProgram,
    Shell,
    Command,
}

/// A candidate way to execute the repository. `path` is relative to the staged
/// workspace unless stated otherwise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entrypoint {
    pub kind: EntrypointKind,
    pub path: Option<String>,
    pub args: Vec<String>,
    pub reason: String,
}

impl Entrypoint {
    pub fn new(kind: EntrypointKind, path: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            kind,
            path,
            args: Vec::new(),
            reason: reason.into(),
        }
    }

    pub fn with_args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(path) = &self.path {
            parts.push(path.clone());
        }
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestInfo {
    pub path: String,
    pub kind: String,
}

/// The complete deterministic analysis result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionPlan {
    pub schema_version: u32,
    pub tool: String,
    pub tool_version: String,
    pub target: PathBuf,
    pub target_display: String,
    pub repo_name: String,
    pub languages: Vec<Language>,
    pub requirements: Vec<Requirement>,
    pub dependencies: Vec<Dependency>,
    pub entrypoints: Vec<Entrypoint>,
    pub manifests: Vec<ManifestInfo>,
    pub doc_hints: Vec<String>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

impl ResolutionPlan {
    pub fn new(target: PathBuf, repo_name: String) -> Self {
        Self {
            schema_version: crate::SNAPSHOT_SCHEMA_VERSION,
            tool: crate::PRODUCT_NAME.to_string(),
            tool_version: crate::PRODUCT_VERSION.to_string(),
            target_display: target.display().to_string(),
            target,
            repo_name,
            languages: Vec::new(),
            requirements: Vec::new(),
            dependencies: Vec::new(),
            entrypoints: Vec::new(),
            manifests: Vec::new(),
            doc_hints: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn requirement_for(&self, runtime: RuntimeKind) -> Option<&Requirement> {
        self.requirements
            .iter()
            .filter(|r| r.runtime == runtime)
            .max_by_key(|r| r.minimum)
    }

    pub fn has_language(&self, language: Language) -> bool {
        self.languages.contains(&language)
    }
}

/// A runtime found on the host machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRuntime {
    pub runtime: RuntimeKind,
    pub version: Version,
    pub executable: PathBuf,
    pub version_output: String,
    pub source: String,
    /// Related executables such as `javac.exe` or `npm.cmd`.
    pub companions: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Compatible,
    TooOld,
    NoHostRuntime,
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostMatch {
    pub runtime: RuntimeKind,
    pub requirement: Option<String>,
    pub minimum: Option<Version>,
    pub host: Option<HostRuntime>,
    pub status: MatchStatus,
    pub detail: String,
}

impl HostMatch {
    pub fn is_compatible(&self) -> bool {
        self.status == MatchStatus::Compatible
    }
}

/// Knobs that keep execution friendly on small or busy machines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProfile {
    pub low_footprint: bool,
    pub memory_limit_mb: Option<u64>,
    pub parallel_jobs: u32,
    pub advisory: Vec<String>,
}

impl Default for ExecutionProfile {
    fn default() -> Self {
        Self {
            low_footprint: false,
            memory_limit_mb: None,
            parallel_jobs: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(2)
                .min(8),
            advisory: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepPolicy {
    Keep,
    Discard,
    Ask,
}

/// Where a repository came from. Restore uses this to know whether it must
/// recolonise a URL or can reuse a local path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Remote,
    Local,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Remote => "remote",
            SourceKind::Local => "local",
        }
    }
}

/// A runtime that was injected from the host or fetched for a sandbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRecord {
    pub runtime: RuntimeKind,
    pub version: Version,
    pub executable: PathBuf,
    pub home: PathBuf,
    pub from_host: bool,
    pub source: String,
    #[serde(default)]
    pub companions: BTreeMap<String, PathBuf>,
}

impl RuntimeRecord {
    pub fn path_dir(&self) -> PathBuf {
        match self.runtime {
            RuntimeKind::Java => self.home.join("bin"),
            _ => self.home.clone(),
        }
    }

    pub fn companion(&self, name: &str) -> Option<&PathBuf> {
        self.companions
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }
}

/// The exact command a sandbox launches, stored so `restore` can rebuild it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub display: String,
    pub entrypoint: Option<Entrypoint>,
}

impl LaunchRecord {
    pub fn command_line(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Prepared,
    Running,
    Finished,
    Kept,
    Discarded,
    Restored,
    Failed,
}

impl LifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleState::Prepared => "prepared",
            LifecycleState::Running => "running",
            LifecycleState::Finished => "finished",
            LifecycleState::Kept => "kept",
            LifecycleState::Discarded => "discarded",
            LifecycleState::Restored => "restored",
            LifecycleState::Failed => "failed",
        }
    }
}

/// Everything needed to inspect, visualise, tear down or restore a sandbox.
/// This is written to `<sandbox>/sandbox.json` and read by the GUI/MCP.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub schema_version: u32,
    pub id: String,
    pub repo_name: String,
    pub source: String,
    pub source_kind: SourceKind,
    pub created_at: String,
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub plan: ResolutionPlan,
    pub runtimes: Vec<RuntimeRecord>,
    pub environment: BTreeMap<String, String>,
    pub launch: Option<LaunchRecord>,
    pub profile: ExecutionProfile,
    pub lifecycle: LifecycleState,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl SandboxRecord {
    pub fn record_path(root: &std::path::Path) -> PathBuf {
        root.join("sandbox.json")
    }
}
