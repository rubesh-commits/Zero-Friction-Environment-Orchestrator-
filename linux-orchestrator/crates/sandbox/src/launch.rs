use std::path::{Path, PathBuf};
use linuxorchestrator_core::{
    process, Entrypoint, EntrypointKind, Error, LaunchRecord, ResolutionPlan, Result, RuntimeKind,
    RuntimeRecord,
};

use crate::layout::SandboxLayout;

/// Pick the entrypoint the user asked for, or the first deterministic
/// candidate. `index` is 1-based in the CLI; internally we keep it explicit.
pub fn select_entrypoint(plan: &ResolutionPlan, index: Option<usize>) -> Result<Entrypoint> {
    match index {
        Some(index) => plan
            .entrypoints
            .get(index)
            .cloned()
            .ok_or_else(|| {
                Error::NoEntrypoint(format!(
                    "{} (entrypoint index {} is out of range; {} candidate(s) detected)",
                    plan.repo_name,
                    index + 1,
                    plan.entrypoints.len()
                ))
            }),
        None => plan.entrypoints.first().cloned().ok_or_else(|| {
            Error::NoEntrypoint(format!(
                "{} (no conventional entry file, manifest script, README command or Java main class was found)",
                plan.repo_name
            ))
        }),
    }
}

pub fn runtime_for(runtimes: &[RuntimeRecord], kind: RuntimeKind) -> Result<&RuntimeRecord> {
    runtimes
        .iter()
        .find(|runtime| runtime.runtime == kind)
        .ok_or_else(|| Error::MissingRuntime(kind.display_name().to_string()))
}

fn python_program(layout: &SandboxLayout, runtime: &RuntimeRecord) -> PathBuf {
    for name in ["python", "python3"] {
        let candidate = layout.venv.join("bin").join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    runtime.executable.clone()
}

fn classpath(layout: &SandboxLayout) -> String {
    format!(
        "{}:{}",
        layout.classes.display(),
        layout.workspace.display()
    )
}

fn npm_program(runtime: &RuntimeRecord) -> Option<PathBuf> {
    runtime.companion("npm").cloned().or_else(|| {
        let candidate = runtime.home.join("bin").join("npm");
        if candidate.is_file() {
            Some(candidate)
        } else {
            process::which("npm")
        }
    })
}

/// Shell scripts are executed through `/bin/sh`; everything else is launched
/// directly so Rust's argument handling stays exact.
pub fn wrap_batch(program: &Path, args: &[String]) -> (String, Vec<String>) {
    let is_shell = program
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("sh") || ext.eq_ignore_ascii_case("bash"))
        .unwrap_or(false);
    if is_shell {
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(program.display().to_string());
        all.extend(args.iter().cloned());
        ("/bin/sh".to_string(), all)
    } else {
        (
            program.display().to_string(),
            args.iter().cloned().collect(),
        )
    }
}

fn find_workspace_program(workspace: &Path, names: &[&str], fallback: &str) -> Result<PathBuf> {
    for name in names {
        let candidate = workspace.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    process::which(fallback).ok_or_else(|| {
        Error::MissingRuntime(format!(
            "{fallback} (not found in the sandbox or on the host)"
        ))
    })
}

pub fn build_launch(
    layout: &SandboxLayout,
    plan: &ResolutionPlan,
    runtimes: &[RuntimeRecord],
    entrypoint: &Entrypoint,
    extra_args: &[String],
) -> Result<LaunchRecord> {
    let mut args: Vec<String> = Vec::new();
    let mut used_entrypoint_args = false;
    let program: PathBuf = match entrypoint.kind {
        EntrypointKind::PythonScript => {
            let runtime = runtime_for(runtimes, RuntimeKind::Python)?;
            let script = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (Python script has no path)", plan.repo_name))
            })?;
            args.push(script.replace('\\', "/"));
            python_program(layout, runtime)
        }
        EntrypointKind::PythonModule => {
            let runtime = runtime_for(runtimes, RuntimeKind::Python)?;
            let module = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (Python module has no name)", plan.repo_name))
            })?;
            args.push("-m".to_string());
            args.push(module.to_string());
            python_program(layout, runtime)
        }
        EntrypointKind::NodeScript => {
            let runtime = runtime_for(runtimes, RuntimeKind::Node)?;
            let script = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (Node script has no path)", plan.repo_name))
            })?;
            args.push(script.replace('\\', "/"));
            runtime.executable.clone()
        }
        EntrypointKind::NpmScript => {
            let runtime = runtime_for(runtimes, RuntimeKind::Node)?;
            let npm = npm_program(runtime).ok_or_else(|| {
                Error::MissingRuntime("npm for the sandbox Node.js runtime".to_string())
            })?;
            if entrypoint.args.is_empty() {
                let script = entrypoint.path.as_deref().unwrap_or("start");
                args.push("run".to_string());
                args.push(script.to_string());
            } else {
                args.extend(entrypoint.args.iter().cloned());
                used_entrypoint_args = true;
            }
            npm
        }
        EntrypointKind::JavaMain => {
            let runtime = runtime_for(runtimes, RuntimeKind::Java)?;
            let class = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (Java main class is empty)", plan.repo_name))
            })?;
            args.push("-cp".to_string());
            args.push(classpath(layout));
            args.push(class.to_string());
            runtime.executable.clone()
        }
        EntrypointKind::JavaJar => {
            let runtime = runtime_for(runtimes, RuntimeKind::Java)?;
            let jar = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (JAR path is empty)", plan.repo_name))
            })?;
            args.push("-jar".to_string());
            args.push(jar.replace('\\', "/"));
            runtime.executable.clone()
        }
        EntrypointKind::Maven => {
            let program_name = entrypoint.path.as_deref().unwrap_or("mvn");
            let program = find_workspace_program(
                &layout.workspace,
                &[program_name, "mvnw", "mvn"],
                "mvn",
            )?;
            if entrypoint.args.is_empty() {
                args.push("spring-boot:run".to_string());
            } else {
                args.extend(entrypoint.args.iter().cloned());
                used_entrypoint_args = true;
            }
            program
        }
        EntrypointKind::Gradle => {
            let program_name = entrypoint.path.as_deref().unwrap_or("gradle");
            let program = find_workspace_program(
                &layout.workspace,
                &[program_name, "gradlew", "gradle"],
                "gradle",
            )?;
            if entrypoint.args.is_empty() {
                args.push("bootRun".to_string());
            } else {
                args.extend(entrypoint.args.iter().cloned());
                used_entrypoint_args = true;
            }
            program
        }
        EntrypointKind::RustBinary => {
            let runtime = runtime_for(runtimes, RuntimeKind::Rust)?;
            args.push("run".to_string());
            args.push("--".to_string());
            runtime
                .companion("cargo")
                .cloned()
                .unwrap_or_else(|| runtime.executable.clone())
        }
        EntrypointKind::GoProgram => {
            let runtime = runtime_for(runtimes, RuntimeKind::Go)?;
            args.push("run".to_string());
            args.push(".".to_string());
            runtime.executable.clone()
        }
        EntrypointKind::Shell | EntrypointKind::Command => {
            let command = entrypoint.path.as_deref().ok_or_else(|| {
                Error::NoEntrypoint(format!("{} (shell command is empty)", plan.repo_name))
            })?;
            args.push("-c".to_string());
            args.push(command.to_string());
            PathBuf::from("/bin/sh")
        }
    };

    if !used_entrypoint_args {
        args.extend(entrypoint.args.iter().cloned());
    }
    args.extend(extra_args.iter().cloned());

    let (program, args) = wrap_batch(&program, &args);
    let display = process::format_command(&program, &args);

    Ok(LaunchRecord {
        program,
        args,
        cwd: layout.workspace.clone(),
        display,
        entrypoint: Some(entrypoint.clone()),
    })
}
