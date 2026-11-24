use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use winorchestrator_core::fsutil;
use winorchestrator_core::process::{self, RunOptions};
use winorchestrator_core::{
    DependencyOrigin, Entrypoint, EntrypointKind, Error, ResolutionPlan, Result, RuntimeKind,
    RuntimeRecord,
};

use crate::env::SandboxEnvironment;
use crate::layout::SandboxLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupMode {
    Auto,
    Skip,
}

#[derive(Clone, Debug)]
pub struct SetupStep {
    pub name: String,
    pub command: String,
    pub success: bool,
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
            success: true,
            detail: detail.into(),
        });
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    fn note(&mut self, message: impl Into<String>) {
        self.notes.push(message.into());
    }
}

fn run_command(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &SandboxEnvironment,
    timeout: Duration,
) -> Result<process::CommandOutcome> {
    let program_text = program.display().to_string();
    let args_ref: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
    let mut options = RunOptions::default().timeout(timeout).cwd(cwd);
    options.env = env.pairs();
    let outcome = process::run_capture(&program_text, &args_ref, &options);
    if outcome.spawn_error.is_some() {
        return Err(Error::Message(format!(
            "could not start {}: {}",
            program.display(),
            outcome.spawn_error.unwrap_or_default()
        )));
    }
    Ok(outcome)
}

fn command_text(program: &Path, args: &[String]) -> String {
    let args: Vec<String> = args.to_vec();
    process::format_command(&program.display().to_string(), &args)
}

fn tail(text: &str, lines: usize) -> String {
    let collected: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    collected
        .iter()
        .skip(collected.len().saturating_sub(lines))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

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
        report.note("Dependency setup was skipped (--no-setup); the project runs against the sandboxed runtimes only.");
        return Ok(report);
    }
    let env = SandboxEnvironment::build(layout, runtimes, &Default::default());
    let needs_python = plan
        .languages
        .iter()
        .any(|language| language.runtime() == Some(RuntimeKind::Python))
        || plan
            .requirements
            .iter()
            .any(|requirement| requirement.runtime == RuntimeKind::Python);
    if needs_python {
        setup_python(layout, plan, runtimes, &env, offline, &mut report)?;
    }
    let needs_node = plan
        .languages
        .iter()
        .any(|language| matches!(language.runtime(), Some(RuntimeKind::Node)));
    if needs_node {
        setup_node(layout, runtimes, &env, offline, &mut report)?;
    }
    if entrypoint.kind == EntrypointKind::JavaMain {
        setup_java(layout, runtimes, &env, &mut report)?;
    }
    Ok(report)
}

fn runtime(runtimes: &[RuntimeRecord], kind: RuntimeKind) -> Result<&RuntimeRecord> {
    runtimes
        .iter()
        .find(|runtime| runtime.runtime == kind)
        .ok_or_else(|| Error::MissingRuntime(kind.display_name().to_string()))
}

fn setup_python(
    layout: &SandboxLayout,
    plan: &ResolutionPlan,
    runtimes: &[RuntimeRecord],
    base_env: &SandboxEnvironment,
    offline: bool,
    report: &mut SetupReport,
) -> Result<()> {
    let runtime = runtime(runtimes, RuntimeKind::Python)?;
    let venv_python = layout.venv.join("Scripts").join("python.exe");
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
            base_env,
            Duration::from_secs(600),
        )?;
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

    let env = SandboxEnvironment::build(layout, runtimes, &Default::default());
    let mut install_args = vec![
        "-m".to_string(),
        "pip".to_string(),
        "install".to_string(),
        "--disable-pip-version-check".to_string(),
        "--no-input".to_string(),
    ];

    let requirements = layout.workspace.join("requirements.txt");
    let constraints = layout.workspace.join("constraints.txt");
    if requirements.is_file() {
        install_args.push("-r".to_string());
        install_args.push("requirements.txt".to_string());
        if constraints.is_file() {
            install_args.push("-c".to_string());
            install_args.push("constraints.txt".to_string());
        }
    } else {
        let specs = python_dependency_specs(plan);
        if specs.is_empty() {
            report.note("No Python dependency list was declared; the venv contains only the runtime's bundled tooling.");
            return Ok(());
        }
        install_args.extend(specs);
    }

    if offline {
        install_args.push("--no-index".to_string());
        report.warn(
            "Offline mode is on: Python package installation is limited to locally cached wheels.",
        );
    }

    let outcome = run_command(
        &venv_python,
        &install_args,
        &layout.workspace,
        &env,
        Duration::from_secs(30 * 60),
    )?;
    let text = command_text(&venv_python, &install_args);
    if !outcome.success {
        return Err(Error::Message(format!(
            "Python dependency installation failed with `{text}`:\n{}",
            tail(&outcome.combined(), 30)
        )));
    }
    report.ok(
        "python-pip",
        &text,
        "installed declared Python dependencies into the sandbox venv",
    );
    Ok(())
}

fn python_dependency_specs(plan: &ResolutionPlan) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut specs = Vec::new();
    for dependency in &plan.dependencies {
        if dependency.origin != DependencyOrigin::Manifest {
            continue;
        }
        if !matches!(
            dependency.kind,
            winorchestrator_core::DependencyKind::Runtime
                | winorchestrator_core::DependencyKind::Build
        ) {
            continue;
        }
        let name = dependency.name.trim();
        if name.is_empty() {
            continue;
        }
        // Import names occasionally leak in from AST analysis; only install
        // names that look like distribution specifiers.
        if name.contains('/') || name.contains('\\') || name == "python" {
            continue;
        }
        let mut spec = name.to_string();
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
        if seen.insert(spec.to_ascii_lowercase()) {
            specs.push(spec);
        }
    }
    specs.sort();
    specs
}

fn setup_node(
    layout: &SandboxLayout,
    runtimes: &[RuntimeRecord],
    env: &SandboxEnvironment,
    offline: bool,
    report: &mut SetupReport,
) -> Result<()> {
    let package_json = layout.workspace.join("package.json");
    if !package_json.is_file() {
        return Ok(());
    }
    let node_modules = layout.workspace.join("node_modules");
    if node_modules.is_dir() {
        report.note("node_modules already exists in the staged workspace; skipping npm install.");
        return Ok(());
    }
    let runtime = runtime(runtimes, RuntimeKind::Node)?;
    let npm = runtime
        .companion("npm.cmd")
        .cloned()
        .or_else(|| {
            let candidate = runtime.home.join("npm.cmd");
            candidate.is_file().then_some(candidate)
        })
        .ok_or_else(|| {
            Error::MissingRuntime("npm.cmd (the sandbox Node.js runtime has no npm)".to_string())
        })?;
    let mut args = vec![
        "install".to_string(),
        "--no-audit".to_string(),
        "--no-fund".to_string(),
    ];
    if offline {
        args.push("--offline".to_string());
        report.warn("Offline mode is on: npm install will fail unless the package cache is already populated.");
    }
    let outcome = run_command(
        &npm,
        &args,
        &layout.workspace,
        env,
        Duration::from_secs(30 * 60),
    )?;
    let text = command_text(&npm, &args);
    if !outcome.success {
        return Err(Error::Message(format!(
            "Node dependency installation failed with `{text}`:\n{}",
            tail(&outcome.combined(), 30)
        )));
    }
    report.ok(
        "node-npm",
        &text,
        "installed package.json dependencies into the sandbox workspace",
    );
    Ok(())
}

fn setup_java(
    layout: &SandboxLayout,
    runtimes: &[RuntimeRecord],
    env: &SandboxEnvironment,
    report: &mut SetupReport,
) -> Result<()> {
    let runtime = runtime(runtimes, RuntimeKind::Java)?;
    let javac = runtime
        .companion("javac.exe")
        .cloned()
        .or_else(|| {
            let candidate = runtime.home.join("bin").join("javac.exe");
            candidate.is_file().then_some(candidate)
        })
        .ok_or_else(|| {
            Error::MissingRuntime(
                "javac.exe (a JDK, not a JRE, is required to compile Java sources)".to_string(),
            )
        })?;
    let sources = find_sources(&layout.workspace, "java")?;
    if sources.is_empty() {
        return Err(Error::NoEntrypoint(
            "the Java entrypoint was detected but no .java sources were found".to_string(),
        ));
    }
    fsutil::ensure_dir(&layout.classes)?;
    let argfile = layout.meta.join("javac.args");
    let mut argfile_text = String::from("-encoding UTF-8\n");
    for source in &sources {
        let text = source.display().to_string().replace('\\', "/");
        argfile_text.push('"');
        argfile_text.push_str(&text.replace('"', "\\\""));
        argfile_text.push_str("\"\n");
    }
    fsutil::write_utf8(&argfile, &argfile_text)?;

    let args = vec![
        "-encoding".to_string(),
        "UTF-8".to_string(),
        "-d".to_string(),
        layout.classes.display().to_string(),
        format!("@{}", argfile.display()),
    ];
    let outcome = run_command(
        &javac,
        &args,
        &layout.workspace,
        env,
        Duration::from_secs(15 * 60),
    )?;
    let text = command_text(&javac, &args);
    if !outcome.success {
        return Err(Error::Message(format!(
            "javac failed with `{text}`:\n{}",
            tail(&outcome.combined(), 30)
        )));
    }
    report.ok(
        "java-compile",
        &text,
        format!("compiled {} Java source file(s)", sources.len()),
    );
    Ok(())
}

fn find_sources(root: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|source| Error::Io {
            context: format!(" while reading {}", dir.display()),
            source,
        })? {
            let entry = entry.map_err(|source| Error::Io {
                context: format!(" while reading {}", dir.display()),
                source,
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if matches!(
                    name.as_str(),
                    "target" | "build" | "out" | "classes" | ".git" | ".gradle"
                ) {
                    continue;
                }
                stack.push(path);
            } else if path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(extension))
                .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_specs_are_deterministic() {
        let mut plan = ResolutionPlan::new(PathBuf::from("C:\\repo"), "repo".to_string());
        plan.dependencies.push(winorchestrator_core::Dependency {
            name: "requests".to_string(),
            requirement: Some(">=2.31".to_string()),
            kind: winorchestrator_core::DependencyKind::Runtime,
            source_file: "requirements.txt".to_string(),
            origin: DependencyOrigin::Manifest,
        });
        plan.dependencies.push(winorchestrator_core::Dependency {
            name: "requests".to_string(),
            requirement: Some(">=2.31".to_string()),
            kind: winorchestrator_core::DependencyKind::Runtime,
            source_file: "pyproject.toml".to_string(),
            origin: DependencyOrigin::Manifest,
        });
        assert_eq!(python_dependency_specs(&plan), vec!["requests>=2.31"]);
    }
}
