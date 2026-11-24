pub mod env;
pub mod launch;
pub mod layout;

use std::process::Command;

pub fn build_ephemeral_sandbox(path: &str) {
    let canonical = std::path::Path::new(path).canonicalize().ok();
    if canonical.is_none() {
        eprintln!("Failed to canonicalize path: {path}");
        return;
    }
    let canonical = canonical.unwrap();
    let dir_path = if canonical.is_file() {
        canonical.parent().map(|p| p.to_path_buf()).unwrap_or(canonical)
    } else {
        canonical
    };
    let abs_dir = dir_path.display().to_string();

    let status = Command::new("bwrap")
        .arg("--ro-bind").arg("/").arg("/")
        .arg("--dev").arg("/dev")
        .arg("--proc").arg("/proc")
        .arg("--bind").arg(&abs_dir).arg(&abs_dir)
        .arg("--chdir").arg(&abs_dir)
        .arg("--unshare-all")
        .arg("--share-net")
        .arg("--hostname").arg("sandbox")
        .arg("--die-with-parent")
        .status();

    match status {
        Ok(stat) => println!("bwrap sandbox exited with: {stat}"),
        Err(_) => println!("bwrap not available, run directly"),
    }
}

/// Execute a launch record in the sandbox environment.
pub fn execute_in_env(
    record: &linuxorchestrator_core::LaunchRecord,
    env: &env::SandboxEnvironment,
    cwd: &std::path::Path,
    timeout: Option<std::time::Duration>,
) -> Result<std::process::ExitStatus, linuxorchestrator_core::Error> {
    let mut cmd = Command::new(&record.program);
    cmd.args(&record.args).current_dir(cwd);
    env.apply(&mut cmd);
    let _ = timeout;
    cmd.status().map_err(|source| linuxorchestrator_core::Error::Io {
        context: format!("running {}", record.display),
        source,
    })
}
