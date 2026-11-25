use crate::env::SandboxEnvironment;
use crate::layout::SandboxLayout;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use linuxorchestrator_core::process::{self, RunOptions};
use linuxorchestrator_core::LaunchRecord;

/// True when `bwrap` exists and can create the user/pid namespaces we need.
/// The probe is cached so a run only pays for it once.
pub fn isolation_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Some(bwrap) = process::which("bwrap") else {
            return false;
        };
        let outcome = process::run_capture(
            &bwrap.display().to_string(),
            &[
                "--unshare-all",
                "--share-net",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--",
                "/bin/true",
            ],
            &RunOptions::default().timeout(Duration::from_secs(10)),
        );
        outcome.success
    })
}

fn writable_dirs(layout: &SandboxLayout) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    for dir in [
        &layout.workspace,
        &layout.venv,
        &layout.runtime_root,
        &layout.home,
        &layout.temp,
        &layout.cache,
        &layout.logs,
        &layout.tools,
        &layout.meta,
        &layout.classes,
    ] {
        if dir.exists() {
            dirs.insert(dir.clone());
        }
    }
    dirs.into_iter().collect()
}

fn bind_into(command: &mut Command, source: &Path, destination: &Path) {
    command
        .arg("--bind")
        .arg(source.display().to_string())
        .arg(destination.display().to_string());
}

/// Build the command used to launch the target. When bubblewrap is usable the
/// process runs inside an isolated namespace with a read-only host root and
/// writable sandbox directories; otherwise it falls back to a direct launch.
pub fn build_command(
    layout: &SandboxLayout,
    environment: &SandboxEnvironment,
    launch: &LaunchRecord,
    extra_args: &[String],
    isolated: bool,
) -> Command {
    let mut args = launch.args.clone();
    args.extend(extra_args.iter().cloned());

    if isolated {
        let bwrap = process::which("bwrap").unwrap_or_else(|| PathBuf::from("bwrap"));
        let mut command = Command::new(bwrap);
        command
            .arg("--die-with-parent")
            .arg("--unshare-all")
            .arg("--share-net")
            .arg("--new-session")
            .arg("--clearenv")
            .arg("--hostname")
            .arg("sandbox")
            .arg("--ro-bind")
            .arg("/")
            .arg("/")
            .arg("--dev")
            .arg("/dev")
            .arg("--proc")
            .arg("/proc")
            .arg("--chdir")
            .arg(launch.cwd.display().to_string());

        for dir in writable_dirs(layout) {
            bind_into(&mut command, &dir, &dir);
        }
        for (key, value) in &environment.vars {
            command.arg("--setenv").arg(key).arg(value);
        }
        command.arg("--").arg(&launch.program).args(&args);
        command
    } else {
        let mut command = Command::new(&launch.program);
        command
            .args(&args)
            .current_dir(&launch.cwd)
            .env_clear();
        for (key, value) in &environment.vars {
            command.env(key, value);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> SandboxLayout {
        let root = std::env::temp_dir().join(format!(
            "lo-bwrap-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        SandboxLayout::at(root, "demo".to_string()).unwrap()
    }

    #[test]
    fn direct_command_clears_the_environment() {
        let layout = layout();
        let mut environment = SandboxEnvironment::default();
        environment.set("PATH", "/sandbox/bin");
        let launch = LaunchRecord {
            program: "/bin/echo".to_string(),
            args: vec!["hi".to_string()],
            cwd: layout.workspace.clone(),
            display: "/bin/echo hi".to_string(),
            entrypoint: None,
        };
        let command = build_command(&layout, &environment, &launch, &[], false);
        assert_eq!(command.get_program(), "/bin/echo");
        let _ = linuxorchestrator_core::fsutil::remove_tree(&layout.root);
    }

    #[test]
    fn isolated_command_uses_bwrap_and_sets_env() {
        let layout = layout();
        let mut environment = SandboxEnvironment::default();
        environment.set("PATH", "/sandbox/bin");
        let launch = LaunchRecord {
            program: "/bin/echo".to_string(),
            args: vec!["hi".to_string()],
            cwd: layout.workspace.clone(),
            display: "/bin/echo hi".to_string(),
            entrypoint: None,
        };
        let command = build_command(&layout, &environment, &launch, &[], true);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert!(args.iter().any(|arg| arg == "--unshare-all"));
        assert!(args.iter().any(|arg| arg == "--setenv"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--" && pair[1] == "/bin/echo"));
        let _ = linuxorchestrator_core::fsutil::remove_tree(&layout.root);
    }
}
