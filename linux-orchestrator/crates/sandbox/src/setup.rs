use crate::env::SandboxEnvironment;
use crate::layout::SandboxLayout;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::process::{self, CommandOutcome, RunOptions};
use linuxorchestrator_core::{
    Dependency, DependencyKind, DependencyOrigin, Entrypoint, EntrypointKind, Error, Language,
    ResolutionPlan, Result, RuntimeKind, RuntimeRecord,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupMode {
    Auto,
    Skip,
}

#[derive(Clone, Debug)]
pub struct SetupStep {
    pub name: String,
    pub command: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct SetupReport {
    pub steps: Vec<SetupStep>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl SetupReport {
    fn ok(&mut self, name: &str, command: &str, detail: impl Into<String>) {
        self.steps.push(SetupStep {
            name: name.to_string(),
            command: command.to_string(),
            detail: detail.into(),
        });
    }

    fn note(&mut self, message: impl Into<String>) {
        self.notes.push(message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Prepare dependencies inside the sandbox before the project is launched.
/// This runs on the host with the sandbox environment, so every write still
/// lands inside the staged workspace or the sandbox venv.
pub fn run_setup(
    layout: &SandboxLayout,
    plan: &ResolutionPlan,
    runtimes: &[RuntimeRecord],
    entrypoint: &Entrypoint,
    mode: SetupMode,
    offline: bool,
) -> Result<SetupReport> {
    let mut report = SetupReport::default();
    if mode == SetupMode::Skip {
        report.note(
            "Dependency setup was skipped (--no-setup); the project runs against the sandboxed runtimes only.",
        );
        return Ok(report);
    }

    if runtimes.iter().any(|r| r.runtime == RuntimeKind::Python) {
        setup_python(layout, plan, runtimes, offline, &mut report)?;
    }
    if runtimes.iter().any(|r| r.runtime == RuntimeKind::Node) {
        setup_node(layout, runtimes, offline, &mut report)?;
    }
    if matches!(
        entrypoint.kind,
        EntrypointKind::JavaMain | EntrypointKind::JavaJar
    ) {
        setup_java(layout, runtimes, entrypoint, &mut report)?;
    }
    Ok(report)
}

fn runtime(runtimes: &[RuntimeRecord], kind: RuntimeKind) -> Result<&RuntimeRecord> {
    runtimes
        .iter()
        .find(|runtime| runtime.runtime == kind)
        .ok_or_else(|| Error::MissingRuntime(kind.display_name().to_string()))
}

fn run_command(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &SandboxEnvironment,
    timeout: Duration,
) -> CommandOutcome {
    let refs: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
    process::run_capture(
        &program.display().to_string(),
        &refs,
        &RunOptions {
            cwd: Some(cwd.to_path_buf()),
            env: env.pairs(),
            timeout,
        },
    )
}

fn run_command_streaming(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &SandboxEnvironment,
    timeout: Duration,
) -> CommandOutcome {
    let refs: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
    process::run_capture_streaming(
        &program.display().to_string(),
        &refs,
        &RunOptions {
            cwd: Some(cwd.to_path_buf()),
            env: env.pairs(),
            timeout,
        },
    )
}

/// Reuse the invoking user's pip cache during setup. Package downloads are the
/// slowest part of preparing a Python sandbox; without this every run starts
/// from an empty per-sandbox cache and re-downloads the same large wheels.
fn host_pip_cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join("pip"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".cache").join("pip"))
}

fn command_text(program: &Path, args: &[String]) -> String {
    process::format_command(&program.display().to_string(), args)
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

fn setup_python(
    layout: &SandboxLayout,
    plan: &ResolutionPlan,
    runtimes: &[RuntimeRecord],
    offline: bool,
    report: &mut SetupReport,
) -> Result<()> {
    let runtime = runtime(runtimes, RuntimeKind::Python)?;
    let venv_python = layout.venv.join("bin").join("python");
    let base_env = SandboxEnvironment::build(layout, runtimes, &Default::default());

    if !venv_python.is_file() {
        let args = vec![
            "-m".to_string(),
            "venv".to_string(),
            layout.venv.display().to_string(),
        ];
        let outcome = run_command(
            &runtime.executable,
            &args,
            &layout.workspace,
            &base_env,
            Duration::from_secs(600),
        );
        let text = command_text(&runtime.executable, &args);
        if !outcome.success {
            return Err(Error::Message(format!(
                "could not create the isolated Python environment with `{text}`:\n{}",
                tail(&outcome.combined(), 20)
            )));
        }
        report.ok(
            "python-venv",
            &text,
            format!("created {}", layout.venv.display()),
        );
    }

    let mut env = SandboxEnvironment::build(layout, runtimes, &Default::default());
    if let Some(cache) = host_pip_cache_dir() {
        env.set("PIP_CACHE_DIR", cache.display().to_string());
    }
    let specs = python_dependency_specs(plan, &layout.workspace);

    let requirements = layout.workspace.join("requirements.txt");
    let constraints = layout.workspace.join("constraints.txt");

    // Build one install unit per source. A unit is either the requirements file
    // (with optional constraints) or a single inferred package spec. Keeping
    // units separate lets us fall back to installing them one at a time when a
    // batch fails, so one unresolvable or bogus package cannot block every
    // other dependency (for example a decoy import).
    let mut units: Vec<(String, Vec<String>)> = Vec::new();
    if requirements.is_file() {
        let mut unit = vec!["-r".to_string(), "requirements.txt".to_string()];
        if constraints.is_file() {
            unit.push("-c".to_string());
            unit.push("constraints.txt".to_string());
        }
        units.push(("requirements.txt".to_string(), unit));
    }
    for spec in &specs {
        units.push((spec.clone(), vec![spec.clone()]));
    }
    if units.is_empty() {
        report.note(
            "No Python dependency list was declared; the venv contains only the runtime's bundled tooling.",
        );
        return Ok(());
    }

    let pip_base: Vec<String> = vec![
        "-m".to_string(),
        "pip".to_string(),
        "install".to_string(),
        "--disable-pip-version-check".to_string(),
        "--no-input".to_string(),
        "--progress-bar".to_string(),
        "on".to_string(),
    ];

    if offline {
        for (_, unit) in units.iter_mut() {
            unit.push("--no-index".to_string());
        }
        report.warn(
            "Offline mode is on: Python package installation is limited to locally cached wheels.",
        );
    }

    let with_base = |unit: &[String]| {
        let mut args = pip_base.clone();
        args.extend(unit.iter().cloned());
        args
    };

    let combined_args: Vec<String> = units
        .iter()
        .flat_map(|(_, unit)| unit.iter().cloned())
        .collect();
    let combined_args = with_base(&combined_args);
    let combined_text = command_text(&venv_python, &combined_args);

    eprintln!(
        "  ... installing {} Python dependency source(s); large wheels can take a while on slow connections",
        units.len()
    );

    let outcome = run_command_streaming(
        &venv_python,
        &combined_args,
        &layout.workspace,
        &env,
        Duration::from_secs(30 * 60),
    );
    if outcome.success {
        report.ok(
            "python-pip",
            &combined_text,
            "installed declared Python dependencies into the sandbox venv",
        );
        return Ok(());
    }

    // A very common failure for older projects is that their pins predate the
    // host Python: there are no wheels for the exact versions and building from
    // source needs system libraries. Retry the same packages without version
    // pins so pip can pick the newest release that works on this runtime.
    if looks_like_version_incompatibility(&outcome.combined()) {
        let relaxed = relaxed_specs(&specs);
        if !relaxed.is_empty() {
            let mut args = pip_base.clone();
            args.extend(relaxed.iter().cloned());
            args.push("--upgrade".to_string());
            if offline {
                args.push("--no-index".to_string());
            }
            eprintln!(
                "  ... pinned versions cannot be built for Python {}; retrying with latest compatible releases",
                runtime.version
            );
            let relaxed_outcome = run_command_streaming(
                &venv_python,
                &args,
                &layout.workspace,
                &env,
                Duration::from_secs(30 * 60),
            );
            if relaxed_outcome.success {
                report.warn(format!(
                    "Pinned Python versions were not installable on Python {} (no matching wheels or missing build headers); installed the latest compatible releases instead: {}.",
                    runtime.version,
                    relaxed.join(", ")
                ));
                report.ok(
                    "python-pip",
                    &command_text(&venv_python, &args),
                    format!(
                        "installed latest compatible releases for {}",
                        relaxed.join(", ")
                    ),
                );
                return Ok(());
            }
        }
    }

    // The batch failed (commonly because a single spec has no matching
    // distribution). Retry each unit independently and keep whatever succeeds.
    report.warn(format!(
        "Batch Python installation failed; retrying dependencies individually so one bad package cannot block the rest:\n{}",
        tail(&outcome.combined(), 15)
    ));

    let mut installed = Vec::new();
    let mut failed = Vec::new();
    for (label, unit) in &units {
        let args = with_base(unit);
        eprintln!("  ... installing Python dependency `{label}`");
        let outcome = run_command_streaming(
            &venv_python,
            &args,
            &layout.workspace,
            &env,
            Duration::from_secs(30 * 60),
        );
        if outcome.success {
            installed.push(label.clone());
        } else {
            failed.push((label.clone(), tail(&outcome.combined(), 8)));
        }
    }

    for (label, detail) in &failed {
        report.warn(format!(
            "could not install Python dependency `{label}`; continuing without it:\n{detail}"
        ));
    }

    if installed.is_empty() {
        report.warn(
            "No Python dependency could be installed; the project runs with the runtime's bundled tooling only.",
        );
    } else {
        report.ok(
            "python-pip",
            &combined_text,
            format!(
                "installed {} of {} Python dependency source(s) into the sandbox venv ({} skipped)",
                installed.len(),
                units.len(),
                failed.len()
            ),
        );
    }
    Ok(())
}

const PYPI_ALIASES: &[(&str, &str)] = &[
    ("bs4", "beautifulsoup4"),
    ("cv2", "opencv-python-headless"),
    ("dateutil", "python-dateutil"),
    ("dotenv", "python-dotenv"),
    ("fitz", "PyMuPDF"),
    ("jwt", "PyJWT"),
    ("magic", "python-magic"),
    ("MySQLdb", "mysqlclient"),
    ("OpenSSL", "pyOpenSSL"),
    ("PIL", "Pillow"),
    ("psycopg2", "psycopg2-binary"),
    ("serial", "pyserial"),
    ("sklearn", "scikit-learn"),
    ("yaml", "PyYAML"),
    ("OpenGL", "PyOpenGL"),
    ("obelix", "obelix-data"),
];

/// Return true when an NVIDIA GPU appears to be available inside the sandbox.
/// The sandbox does a read‑only bind of the host filesystem, so `/dev/nvidia0`
/// is visible when the host has a driver loaded.
fn host_has_nvidia_gpu() -> bool {
    Path::new("/dev/nvidia0").exists()
}

fn pypi_name(import_name: &str) -> String {
    for (from, to) in PYPI_ALIASES {
        if import_name.eq_ignore_ascii_case(from) {
            return (*to).to_string();
        }
    }
    import_name.to_string()
}

fn is_local_module(workspace: &Path, name: &str) -> bool {
    if workspace.join(format!("{name}.py")).is_file() {
        return true;
    }
    let dir = workspace.join(name);
    if !dir.is_dir() {
        return false;
    }
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .any(|entry| entry.path().join("__init__.py").is_file()
                    || entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("py"))
                        .unwrap_or(false))
        })
        .unwrap_or(false)
}

/// True when a dependency belongs to the Python ecosystem. Dependencies from
/// older restore files have no `language`, so we infer it from the declaring
/// manifest or source file.
fn is_python_dependency(dependency: &Dependency) -> bool {
    if let Some(language) = dependency.language {
        return language == Language::Python;
    }
    let file = dependency
        .source_file
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if dependency.origin == DependencyOrigin::Ast {
        return file.ends_with(".py") || file.ends_with(".pyw");
    }
    matches!(
        file.as_str(),
        "requirements.txt"
            | "requirements-dev.txt"
            | "constraints.txt"
            | "pyproject.toml"
            | "setup.py"
            | "setup.cfg"
    ) || (file.starts_with("requirements") && file.ends_with(".txt"))
}

fn python_dependency_specs(plan: &ResolutionPlan, workspace: &Path) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut specs = Vec::new();
    for dependency in &plan.dependencies {
        if !matches!(
            dependency.kind,
            DependencyKind::Runtime | DependencyKind::Build
        ) {
            continue;
        }
        if !is_python_dependency(dependency) {
            continue;
        }
        let mut name = dependency.name.trim().to_string();
        if name.is_empty() || name.contains('/') || name.contains('\\') || name == "python" {
            continue;
        }
        // Dotted imports (`package.module`) are installed by their top-level
        // distribution name.
        if let Some((head, _)) = name.split_once('.') {
            name = head.to_string();
        }
        if name.is_empty() {
            continue;
        }
        if dependency.origin == DependencyOrigin::Ast && is_local_module(workspace, &name) {
            continue;
        }
        let mut spec = pypi_name(&name);
        if dependency.origin == DependencyOrigin::Manifest {
            if let Some(requirement) = dependency.requirement.as_deref() {
                let requirement = requirement.trim();
                if !requirement.is_empty()
                    && requirement != "*"
                    && requirement
                        .chars()
                        .next()
                        .map(|ch| matches!(ch, '<' | '>' | '=' | '!' | '~' | '^'))
                        .unwrap_or(false)
                {
                    spec.push_str(requirement);
                }
            }
        }
        if seen.insert(spec.to_ascii_lowercase()) {
            specs.push(spec.to_ascii_lowercase());
        }
    }
    specs.sort();
    specs
}

/// Strip a version specifier and extras from a requirement, leaving only the
/// distribution name. Used to retry a dependency set without exact pins.
fn distribution_name(spec: &str) -> String {
    let end = spec
        .find(|ch: char| {
            matches!(
                ch,
                '<' | '>' | '=' | '!' | '~' | '^' | '[' | ';' | ' ' | '(' | '@'
            )
        })
        .unwrap_or(spec.len());
    spec[..end].trim().to_string()
}

fn relaxed_specs(specs: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut relaxed = Vec::new();
    for spec in specs {
        let name = distribution_name(spec);
        if !name.is_empty() && seen.insert(name.to_ascii_lowercase()) {
            relaxed.push(name);
        }
    }
    relaxed
}

/// Heuristics for "these pinned versions cannot be installed on this Python",
/// as opposed to a transient network failure or a genuinely unknown package.
fn looks_like_version_incompatibility(output: &str) -> bool {
    [
        "Failed to build",
        "Getting requirements to build wheel did not run successfully",
        "subprocess-exited-with-error",
        "requires a different Python",
        "No matching distribution found",
        "Could not find a version that satisfies",
        "no wheels with a matching Python",
    ]
    .iter()
    .any(|needle| output.contains(needle))
}

fn setup_node(
    layout: &SandboxLayout,
    runtimes: &[RuntimeRecord],
    offline: bool,
    report: &mut SetupReport,
) -> Result<()> {
    let package_dirs = find_package_json_dirs(&layout.workspace)?;
    if package_dirs.is_empty() {
        report.note("No package.json was found; skipping npm install.");
        return Ok(());
    }
    let runtime = runtime(runtimes, RuntimeKind::Node)?;
    let npm = runtime
        .companion("npm")
        .cloned()
        .or_else(|| {
            let candidate = runtime.home.join("bin").join("npm");
            candidate.is_file().then_some(candidate)
        })
        .or_else(|| process::which("npm"))
        .ok_or_else(|| {
            Error::MissingRuntime("npm (the sandbox Node.js runtime has no npm)".to_string())
        })?;
    let mut args = vec![
        "install".to_string(),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
    ];
    if offline {
        args.push("--offline".to_string());
        report.warn(
            "Offline mode is on: npm install will fail unless the package cache is already populated.",
        );
    }
    let env = SandboxEnvironment::build(layout, runtimes, &Default::default());
    let text = command_text(&npm, &args);
    let mut installed = Vec::new();
    for dir in package_dirs {
        if dir.join("node_modules").is_dir() {
            let label = relative_label(&layout.workspace, &dir);
            report.note(format!(
                "node_modules already exists in {label}; skipping npm install there."
            ));
            continue;
        }
        let label = relative_label(&layout.workspace, &dir);
        eprintln!("  ... installing Node dependencies in {label}");
        let outcome = run_command_streaming(
            &npm,
            &args,
            &dir,
            &env,
            Duration::from_secs(30 * 60),
        );
        if !outcome.success {
            return Err(Error::Message(format!(
                "Node dependency installation failed in {label} with `{text}`:\n{}",
                tail(&outcome.combined(), 30)
            )));
        }
        installed.push(label);
    }
    if installed.is_empty() {
        report.note("Every discovered package.json already has node_modules; skipping npm install.");
    } else {
        report.ok(
            "node-npm",
            &text,
            format!(
                "installed Node dependencies in {}",
                installed.join(", ")
            ),
        );
    }
    Ok(())
}

/// Discover every directory that declares a `package.json`, skipping dependency
/// and build output trees. Repositories sometimes run Node code from a
/// subdirectory (for example `node_modules_test/`) without a root manifest, so
/// installing only at the workspace root leaves those scripts without modules.
fn find_package_json_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "node_modules"
                    | ".git"
                    | ".hg"
                    | ".svn"
                    | "target"
                    | "dist"
                    | "build"
                    | "out"
                    | ".gradle"
                    | ".cache"
            )
        })
        .filter_map(|entry| entry.ok())
    {
        if entry.file_type().is_file() && entry.file_name() == "package.json" {
            if let Some(parent) = entry.path().parent() {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

fn relative_label(root: &Path, dir: &Path) -> String {
    let relative = dir.strip_prefix(root).unwrap_or(dir);
    let text = relative.display().to_string();
    if text.is_empty() {
        ".".to_string()
    } else {
        text
    }
}

fn setup_java(
    layout: &SandboxLayout,
    runtimes: &[RuntimeRecord],
    entrypoint: &Entrypoint,
    report: &mut SetupReport,
) -> Result<()> {
    let Ok(runtime) = runtime(runtimes, RuntimeKind::Java) else {
        return Ok(());
    };
    let javac = runtime
        .companion("javac")
        .cloned()
        .or_else(|| {
            let candidate = runtime.home.join("bin").join("javac");
            candidate.is_file().then_some(candidate)
        })
        .or_else(|| process::which("javac"));
    let Some(javac) = javac else {
        report.warn(
            "javac was not found; Java sources will not be compiled and the entrypoint may need prebuilt classes or a build wrapper.",
        );
        return Ok(());
    };
    let sources = find_sources(&layout.workspace, "java")?;
    if sources.is_empty() {
        report.note("No .java sources were found to compile.");
        return Ok(());
    }
    fsutil::ensure_dir(&layout.classes)?;
    let mut args = vec!["-d".to_string(), layout.classes.display().to_string()];
    args.extend(sources.iter().map(|path| path.display().to_string()));
    let env = SandboxEnvironment::build(layout, runtimes, &Default::default());
    let outcome = run_command(
        &javac,
        &args,
        &layout.workspace,
        &env,
        Duration::from_secs(10 * 60),
    );
    let text = command_text(&javac, &args);
    if !outcome.success {
        return Err(Error::Message(format!(
            "Java compilation failed with `{text}`:\n{}",
            tail(&outcome.combined(), 30)
        )));
    }
    report.ok(
        "java-javac",
        &text,
        format!(
            "compiled {} source file(s) for {}",
            sources.len(),
            entrypoint.path.as_deref().unwrap_or("the Java entrypoint")
        ),
    );
    Ok(())
}

fn find_sources(root: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "target" | "build" | "dist" | "out" | ".gradle" | "node_modules" | ".git"
            )
        })
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case(extension))
            .unwrap_or(false)
        {
            sources.push(entry.path().to_path_buf());
        }
    }
    sources.sort();
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxorchestrator_core::Dependency;

    #[test]
    fn maps_import_names_and_skips_local_modules() {
        let workspace = std::env::temp_dir().join(format!(
            "lo-setup-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join("helpers")).unwrap();
        std::fs::write(workspace.join("helpers/__init__.py"), "").unwrap();
        std::fs::write(workspace.join("local.py"), "").unwrap();

        let mut plan = ResolutionPlan::new(workspace.clone(), "demo".to_string());
        for name in ["sklearn", "yaml", "helpers", "local", "requests"] {
            plan.dependencies.push(Dependency {
                name: name.to_string(),
                requirement: None,
                kind: DependencyKind::Runtime,
                source_file: "app.py".to_string(),
                origin: DependencyOrigin::Ast,
                language: Some(Language::Python),
            });
        }
        plan.dependencies.push(Dependency {
            name: "flask".to_string(),
            requirement: Some(">=3.0".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "requirements.txt".to_string(),
            origin: DependencyOrigin::Manifest,
            language: Some(Language::Python),
        });

        let specs = python_dependency_specs(&plan, &workspace);
        assert!(specs.contains(&"scikit-learn".to_string()));
        assert!(specs.contains(&"pyyaml".to_string()));
        assert!(specs.contains(&"requests".to_string()));
        assert!(specs.contains(&"flask>=3.0".to_string()));
        assert!(!specs.iter().any(|spec| spec == "helpers"));
        assert!(!specs.iter().any(|spec| spec == "local"));
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn ignores_non_python_dependencies() {
        let workspace = std::env::temp_dir().join(format!(
            "lo-setup-mixed-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let mut plan = ResolutionPlan::new(workspace.clone(), "demo".to_string());
        plan.dependencies.push(Dependency {
            name: "flask".to_string(),
            requirement: Some(">=3.0".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "requirements.txt".to_string(),
            origin: DependencyOrigin::Manifest,
            language: Some(Language::Python),
        });
        plan.dependencies.push(Dependency {
            name: "axios".to_string(),
            requirement: Some("^1.7.0".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "package.json".to_string(),
            origin: DependencyOrigin::Manifest,
            language: Some(Language::JavaScript),
        });
        plan.dependencies.push(Dependency {
            name: "requests".to_string(),
            requirement: None,
            kind: DependencyKind::Runtime,
            source_file: "web.js".to_string(),
            origin: DependencyOrigin::Ast,
            language: Some(Language::JavaScript),
        });

        let specs = python_dependency_specs(&plan, &workspace);
        assert_eq!(specs, vec!["flask>=3.0".to_string()]);
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn relaxes_pinned_specs_to_distribution_names() {
        let specs = vec![
            "flask==1.1.2".to_string(),
            "lxml==4.5.1".to_string(),
            "requests>=2.0".to_string(),
            "Flask-RESTful[async]==0.3.8".to_string(),
            "flask==2.0".to_string(),
        ];
        assert_eq!(
            relaxed_specs(&specs),
            vec![
                "flask".to_string(),
                "lxml".to_string(),
                "requests".to_string(),
                "Flask-RESTful".to_string(),
            ]
        );
        assert!(looks_like_version_incompatibility(
            "ERROR: Failed to build 'lxml'"
        ));
        assert!(looks_like_version_incompatibility(
            "No matching distribution found for foo"
        ));
        assert!(!looks_like_version_incompatibility(
            "Connection timed out while downloading"
        ));
    }

    #[test]
    fn finds_nested_package_json_directories() {
        let workspace = std::env::temp_dir().join(format!(
            "lo-setup-node-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join("node_modules_test")).unwrap();
        std::fs::create_dir_all(workspace.join("node_modules/ignored")).unwrap();
        std::fs::write(workspace.join("package.json"), "{}").unwrap();
        std::fs::write(workspace.join("node_modules_test/package.json"), "{}").unwrap();
        std::fs::write(workspace.join("node_modules/ignored/package.json"), "{}").unwrap();

        let dirs = find_package_json_dirs(&workspace).unwrap();
        assert!(dirs.contains(&workspace));
        assert!(dirs.contains(&workspace.join("node_modules_test")));
        assert!(!dirs
            .iter()
            .any(|dir| dir.to_string_lossy().contains("node_modules/ignored")));
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn legacy_dependencies_without_language_are_inferred() {
        let workspace = std::env::temp_dir().join(format!(
            "lo-setup-legacy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let mut plan = ResolutionPlan::new(workspace.clone(), "demo".to_string());
        plan.dependencies.push(Dependency {
            name: "numpy".to_string(),
            requirement: None,
            kind: DependencyKind::Runtime,
            source_file: "pyproject.toml".to_string(),
            origin: DependencyOrigin::Manifest,
            language: None,
        });
        plan.dependencies.push(Dependency {
            name: "express".to_string(),
            requirement: Some("^4.18.0".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "package.json".to_string(),
            origin: DependencyOrigin::Manifest,
            language: None,
        });
        plan.dependencies.push(Dependency {
            name: "serde".to_string(),
            requirement: Some("1".to_string()),
            kind: DependencyKind::Runtime,
            source_file: "Cargo.toml".to_string(),
            origin: DependencyOrigin::Manifest,
            language: None,
        });

        let specs = python_dependency_specs(&plan, &workspace);
        assert_eq!(specs, vec!["numpy".to_string()]);
        std::fs::remove_dir_all(&workspace).unwrap();
    }
}
