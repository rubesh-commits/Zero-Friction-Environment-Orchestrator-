use crate::error::{Error, Result};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct CommandOutcome { pub program: String, pub args: Vec<String>, pub success: bool, pub code: Option<i32>, pub stdout: String, pub stderr: String, pub timed_out: bool, pub spawn_error: Option<String> }

impl CommandOutcome {
    pub fn combined(&self) -> String {
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        match (stdout.is_empty(), stderr.is_empty()) { (false, false) => format!("{stdout}\n{stderr}"), (false, true) => stdout.to_string(), (true, false) => stderr.to_string(), (true, true) => String::new() }
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
pub struct RunOptions { pub cwd: Option<PathBuf>, pub env: Vec<(String, String)>, pub timeout: Duration }
impl Default for RunOptions { fn default() -> Self { Self { cwd: None, env: Vec::new(), timeout: Duration::from_secs(30) } } }
impl RunOptions { pub fn timeout(mut self, timeout: Duration) -> Self { self.timeout = timeout; self } pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self { self.cwd = Some(cwd.into()); self } }

pub fn hidden_command(program: impl AsRef<OsStr>) -> Command { Command::new(program) }

pub fn run_capture(program: &str, args: &[&str], options: &RunOptions) -> CommandOutcome {
    run_impl(program, args, options, false)
}

/// Like [`run_capture`], but forwards the child's stdout/stderr to this
/// process' stderr as it arrives. Use this for long-running tools (package
/// managers, compilers) so an interactive user sees progress instead of a
/// silent pause.
pub fn run_capture_streaming(program: &str, args: &[&str], options: &RunOptions) -> CommandOutcome {
    run_impl(program, args, options, true)
}

fn run_impl(program: &str, args: &[&str], options: &RunOptions, stream_pipe: bool) -> CommandOutcome {
    let mut command = hidden_command(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = &options.cwd { command.current_dir(cwd); }
    for (key, value) in &options.env { command.env(key, value); }

    let mut child = match command.spawn() { Ok(child) => child, Err(err) => return CommandOutcome { program: program.to_string(), args: args.iter().map(|a| a.to_string()).collect(), spawn_error: Some(err.to_string()), ..Default::default() } };
    
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = stdout.map(|stream| spawn_reader(stream, stream_pipe));
    let err_reader = stderr.map(|stream| spawn_reader(stream, stream_pipe));

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= options.timeout { timed_out = true; let _ = child.kill(); break child.wait().ok(); }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => { let _ = child.kill(); break child.wait().ok(); }
        }
    };

    let stdout = out_reader.and_then(|h| h.join().ok()).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let stderr = err_reader.and_then(|h| h.join().ok()).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();

    CommandOutcome { program: program.to_string(), args: args.iter().map(|a| a.to_string()).collect(), success: status.map(|s| s.success()).unwrap_or(false), code: status.and_then(|s| s.code()), stdout, stderr, timed_out, spawn_error: None }
}

/// Read a child pipe to completion, optionally echoing each chunk to stderr so
/// the user sees live output. The full contents are still returned so callers
/// can inspect failures after the fact.
fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    echo: bool,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        if !echo {
            let _ = reader.read_to_end(&mut collected);
            return collected;
        }
        use std::io::Write;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    collected.extend_from_slice(&buffer[..n]);
                    let mut target = std::io::stderr();
                    let _ = target.write_all(&buffer[..n]);
                    let _ = target.flush();
                }
                Err(_) => break,
            }
        }
        collected
    })
}

pub fn status_interactive(program: &str, args: &[String], cwd: &Path, envs: &[(String, String)]) -> Result<ExitStatus> {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    for (key, value) in envs { command.env(key, value); }
    command.status().map_err(|source| Error::Io { context: format!(" while launching {}", program), source })
}

pub fn which(program: &str) -> Option<PathBuf> {
    let direct = Path::new(program);
    if direct.is_absolute() || direct.components().count() > 1 { return direct.is_file().then(|| direct.to_path_buf()); }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() { return Some(candidate); }
    }
    None
}

pub fn is_pid_alive(pid: u32) -> bool { crate::linuxapi::is_pid_alive(pid) }

pub fn terminate_pid_tree(pid: u32) -> CommandOutcome {
    run_capture("kill", &["-9", &pid.to_string()], &RunOptions::default().timeout(Duration::from_secs(5)))
}

pub fn open_url(url: &str) -> Result<()> {
    Command::new("xdg-open").arg(url).status().map_err(|source| Error::Io { context: "opening browser".to_string(), source })?;
    Ok(())
}

pub fn format_command(program: &str, args: &[String]) -> String {
    let mut parts = vec![quote(program)];
    parts.extend(args.iter().map(|arg| quote(arg)));
    parts.join(" ")
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '=' | ',' | '+' | '@' | '%'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}