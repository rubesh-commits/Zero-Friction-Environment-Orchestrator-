use crate::error::{Error, Result};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Result of a non-interactive probe command.
#[derive(Clone, Debug, Default)]
pub struct CommandOutcome {
    pub program: String,
    pub args: Vec<String>,
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
}

impl CommandOutcome {
    pub fn combined(&self) -> String {
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        match (stdout.is_empty(), stderr.is_empty()) {
            (false, false) => format!("{stdout}\n{stderr}"),
            (false, true) => stdout.to_string(),
            (true, false) => stderr.to_string(),
            (true, true) => String::new(),
        }
    }

    pub fn first_line(&self) -> String {
        self.combined()
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }
}

impl RunOptions {
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// Build a command with no console window. Probing tools must never flash a
/// cmd window in front of the user.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    hide_window(&mut command);
    command
}

#[cfg(windows)]
pub fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_window(_command: &mut Command) {}

/// Run a probe command with captured output and a hard timeout. Only the
/// probe's own child is killed on timeout; user applications are never touched.
pub fn run_capture(program: &str, args: &[&str], options: &RunOptions) -> CommandOutcome {
    let mut command = hidden_command(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &options.env {
        command.env(key, value);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CommandOutcome {
                program: program.to_string(),
                args: args.iter().map(|a| a.to_string()).collect(),
                spawn_error: Some(err.to_string()),
                ..Default::default()
            }
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = stdout.map(|mut stream| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stream.read_to_end(&mut buffer);
            buffer
        })
    });
    let err_reader = stderr.map(|mut stream| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stream.read_to_end(&mut buffer);
            buffer
        })
    });

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= options.timeout {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                let _ = child.kill();
                break child.wait().ok();
            }
        }
    };

    let stdout = out_reader
        .and_then(|handle| handle.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let stderr = err_reader
        .and_then(|handle| handle.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();

    CommandOutcome {
        program: program.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        success: status.map(|s| s.success()).unwrap_or(false),
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
        spawn_error: None,
    }
}

/// Run a command attached to the current console, inheriting stdin/stdout.
/// This is what the actual sandboxed project sees.
pub fn status_interactive(
    program: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
) -> Result<ExitStatus> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (key, value) in envs {
        command.env(key, value);
    }
    command.status().map_err(|source| Error::Io {
        context: format!(" while launching {}", program),
        source,
    })
}

/// Find an executable on PATH, including PATHEXT lookup.
pub fn which(program: &str) -> Option<PathBuf> {
    let direct = Path::new(program);
    if direct.is_absolute() || direct.components().count() > 1 {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let extensions = path_extensions();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        if Path::new(program).extension().is_none() {
            for extension in &extensions {
                let candidate = dir.join(format!("{program}{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub fn which_all(program: &str) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    let extensions = path_extensions();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(program);
            if candidate.is_file() {
                found.insert(candidate);
            }
            if Path::new(program).extension().is_none() {
                for extension in &extensions {
                    let candidate = dir.join(format!("{program}{extension}"));
                    if candidate.is_file() {
                        found.insert(candidate);
                    }
                }
            }
        }
    }
    found.into_iter().collect()
}

fn path_extensions() -> Vec<String> {
    let raw =
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.JS".to_string());
    raw.split(';')
        .filter(|ext| !ext.trim().is_empty())
        .map(|ext| ext.trim().to_string())
        .collect()
}

pub fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

pub fn is_pid_alive(pid: u32) -> bool {
    crate::winapi::is_pid_alive(pid)
}

/// Terminate a sandbox process tree by PID. This is only exposed for explicit
/// stale-sandbox cleanup after user confirmation; it is never called for
/// arbitrary host applications.
pub fn terminate_pid_tree(pid: u32) -> CommandOutcome {
    run_capture(
        "taskkill.exe",
        &["/PID", &pid.to_string(), "/T", "/F"],
        &RunOptions::default().timeout(Duration::from_secs(15)),
    )
}

pub fn open_url(url: &str) -> Result<()> {
    let mut command = hidden_command("cmd.exe");
    command.args(["/C", "start", "", url]);
    command.status().map_err(|source| Error::Io {
        context: " while opening the dashboard in the default browser".to_string(),
        source,
    })?;
    Ok(())
}

pub fn format_command(program: &str, args: &[String]) -> String {
    let mut parts = vec![quote(program)];
    parts.extend(args.iter().map(|arg| quote(arg)));
    parts.join(" ")
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '&' | '|' | '<' | '>' | '^'))
    {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cmd_on_windows() {
        if cfg!(windows) {
            assert!(which("cmd.exe").is_some());
        }
    }

    #[test]
    fn captures_echo_output() {
        if cfg!(windows) {
            let outcome = run_capture(
                "cmd.exe",
                &["/C", "echo hello"],
                &RunOptions::default().timeout(Duration::from_secs(5)),
            );
            assert!(outcome.success);
            assert!(outcome.combined().to_lowercase().contains("hello"));
        }
    }
}
