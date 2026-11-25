use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use linuxorchestrator_core::linuxapi::{self, DiskSpace, MemoryStatus, ProcessEntry};
use linuxorchestrator_core::paths;
use linuxorchestrator_core::process::{self, RunOptions};
use linuxorchestrator_core::{
    ExecutionProfile, HostMatch, HostRuntime, MatchStatus, ResolutionPlan, RuntimeKind, Version,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheInfo {
    pub name: String,
    pub path: PathBuf,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMemory {
    pub name: String,
    pub pid: u32,
    pub working_set_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StaleSandbox {
    pub id: String,
    pub path: PathBuf,
    pub pid: Option<u32>,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProbeReport {
    pub runtimes: Vec<HostRuntime>,
    pub caches: Vec<CacheInfo>,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub memory_load_percent: u32,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
    pub top_memory_consumers: Vec<ProcessMemory>,
    pub profile: ExecutionProfile,
    pub stale_sandboxes: Vec<StaleSandbox>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

impl ProbeReport {
    pub fn runtime(&self, kind: RuntimeKind) -> Option<&HostRuntime> {
        self.runtimes.iter().find(|runtime| runtime.runtime == kind)
    }

    pub fn memory(&self) -> Option<MemoryStatus> {
        (self.memory_total_bytes > 0).then_some(MemoryStatus {
            total_bytes: self.memory_total_bytes,
            available_bytes: self.memory_available_bytes,
            load_percent: self.memory_load_percent,
        })
    }

    pub fn disk(&self) -> Option<DiskSpace> {
        (self.disk_total_bytes > 0).then_some(DiskSpace {
            total_bytes: self.disk_total_bytes,
            free_bytes: self.disk_free_bytes,
            available_bytes: self.disk_free_bytes,
        })
    }
}

/// Probe host runtimes, caches, RAM/disk telemetry and stale sandbox records.
pub fn probe(target: &Path) -> ProbeReport {
    let mut report = ProbeReport::default();
    report.runtimes = probe_runtimes(&mut report.warnings);
    report.caches = probe_caches();
    report.stale_sandboxes = find_stale_sandboxes();

    if let Some(memory) = linuxapi::memory_status() {
        report.memory_total_bytes = memory.total_bytes;
        report.memory_available_bytes = memory.available_bytes;
        report.memory_load_percent = memory.load_percent;
    } else {
        report
            .warnings
            .push("Could not read physical memory telemetry.".to_string());
    }

    let volume = paths::volume_root(target);
    if let Some(disk) = linuxapi::disk_space(&volume) {
        report.disk_total_bytes = disk.total_bytes;
        report.disk_free_bytes = disk.free_bytes;
    } else {
        report.warnings.push(format!(
            "Could not read free space for {}.",
            volume.display()
        ));
    }

    report.profile = build_profile(
        report.memory_available_bytes,
        report.memory_load_percent,
        &mut report.warnings,
    );
    if report.profile.low_footprint {
        report.top_memory_consumers = top_memory_consumers(6);
        if !report.top_memory_consumers.is_empty() {
            report.profile.advisory.push(format!(
                "Top memory consumers (read-only, never terminated): {}.",
                report
                    .top_memory_consumers
                    .iter()
                    .map(|process| format!(
                        "{} ({})",
                        process.name,
                        human_mb(process.working_set_bytes)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        report.profile.advisory.push(
            "Low-footprint mode limits build parallelism and applies a memory cap to sandbox processes only. Your other apps are untouched."
                .to_string(),
        );
    }

    report.notes.push(
        "Host injection is preferred: compatible host runtimes are reused instead of downloaded."
            .to_string(),
    );
    report
}

/// Detailed probe used by `doctor`; includes the read-only memory advisory even
/// when the machine is not under pressure.
pub fn probe_detailed(target: &Path) -> ProbeReport {
    let mut report = probe(target);
    if report.top_memory_consumers.is_empty() {
        report.top_memory_consumers = top_memory_consumers(8);
    }
    report
}

pub fn probe_runtimes(warnings: &mut Vec<String>) -> Vec<HostRuntime> {
    let mut runtimes = Vec::new();
    for kind in [
        RuntimeKind::Python,
        RuntimeKind::Node,
        RuntimeKind::Java,
        RuntimeKind::Rust,
        RuntimeKind::Go,
    ] {
        if let Some(runtime) = probe_runtime(kind, warnings) {
            runtimes.push(runtime);
        }
    }
    runtimes
}

fn probe_runtime(kind: RuntimeKind, warnings: &mut Vec<String>) -> Option<HostRuntime> {
    match kind {
        RuntimeKind::Python => probe_python(warnings),
        RuntimeKind::Node => probe_node(),
        RuntimeKind::Java => probe_java(),
        RuntimeKind::Rust => probe_rust(),
        RuntimeKind::Go => probe_go(),
    }
}

#[derive(Clone)]
struct Candidate {
    program: PathBuf,
    prefix: Vec<String>,
    source: String,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn add_path_candidate(candidates: &mut Vec<Candidate>, program: &str, source: &str) {
    if let Some(found) = process::which(program) {
        candidates.push(Candidate {
            program: found,
            prefix: Vec::new(),
            source: source.to_string(),
        });
    }
}

/// Check `base/<name>` and one level of versioned installs (`base/*/bin/<name>`).
fn add_install_candidates(
    candidates: &mut Vec<Candidate>,
    base: &Path,
    names: &[&str],
    source: &str,
) {
    for name in names {
        let direct = base.join(name);
        if direct.is_file() {
            candidates.push(Candidate {
                program: direct,
                prefix: Vec::new(),
                source: format!("{source} {}", base.display()),
            });
        }
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        if !entry.path().is_dir() {
            continue;
        }
        for name in names {
            let candidate = entry.path().join("bin").join(name);
            if candidate.is_file() {
                candidates.push(Candidate {
                    program: candidate,
                    prefix: Vec::new(),
                    source: format!("{source} {}", entry.path().display()),
                });
            }
        }
    }
}

fn probe_python(warnings: &mut Vec<String>) -> Option<HostRuntime> {
    let mut candidates = Vec::new();
    add_path_candidate(&mut candidates, "python3", "PATH");
    add_path_candidate(&mut candidates, "python", "PATH");

    let mut bases = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = home_dir() {
        bases.push(home.join(".local/bin"));
        bases.push(home.join(".pyenv/versions"));
        bases.push(home.join(".local/share/uv/python"));
    }
    for base in bases {
        add_install_candidates(&mut candidates, &base, &["python3", "python"], "install dir");
    }
    probe_candidates(kind_python(), candidates, warnings)
}

fn probe_node() -> Option<HostRuntime> {
    let mut candidates = Vec::new();
    add_path_candidate(&mut candidates, "node", "PATH");

    let mut bases = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = home_dir() {
        bases.push(home.join(".volta/bin"));
        bases.push(home.join(".nvm/versions/node"));
        bases.push(home.join(".local/share/fnm/node-versions"));
    }
    for base in bases {
        add_install_candidates(&mut candidates, &base, &["node"], "install dir");
    }
    let mut warnings = Vec::new();
    probe_candidates(kind_node(), candidates, &mut warnings)
}

fn probe_java() -> Option<HostRuntime> {
    let mut candidates = Vec::new();
    add_path_candidate(&mut candidates, "java", "PATH");
    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        let exe = PathBuf::from(java_home).join("bin").join("java");
        if exe.is_file() {
            candidates.push(Candidate {
                program: exe,
                prefix: Vec::new(),
                source: "JAVA_HOME".to_string(),
            });
        }
    }

    let mut bases = vec![PathBuf::from("/usr/lib/jvm")];
    if let Some(home) = home_dir() {
        bases.push(home.join(".sdkman/candidates/java"));
        bases.push(home.join(".jdks"));
    }
    for base in bases {
        add_install_candidates(&mut candidates, &base, &["java"], "install dir");
    }
    let mut warnings = Vec::new();
    probe_candidates(kind_java(), candidates, &mut warnings)
}

fn probe_rust() -> Option<HostRuntime> {
    let mut candidates = Vec::new();
    add_path_candidate(&mut candidates, "rustc", "PATH");
    if let Some(home) = home_dir() {
        let rustc = home.join(".cargo").join("bin").join("rustc");
        if rustc.is_file() {
            candidates.push(Candidate {
                program: rustc,
                prefix: Vec::new(),
                source: "~/.cargo/bin".to_string(),
            });
        }
    }
    let mut warnings = Vec::new();
    probe_candidates(kind_rust(), candidates, &mut warnings)
}

fn probe_go() -> Option<HostRuntime> {
    let mut candidates = Vec::new();
    add_path_candidate(&mut candidates, "go", "PATH");
    for base in [PathBuf::from("/usr/local/go/bin"), PathBuf::from("/usr/lib/go/bin")] {
        add_install_candidates(&mut candidates, &base, &["go"], "install dir");
    }
    let mut warnings = Vec::new();
    probe_candidates(kind_go(), candidates, &mut warnings)
}

struct KindSpec {
    runtime: RuntimeKind,
    version_args: &'static [&'static str],
    companion_names: &'static [&'static str],
}

fn kind_python() -> KindSpec {
    KindSpec {
        runtime: RuntimeKind::Python,
        version_args: &["--version"],
        companion_names: &["pip", "pip3"],
    }
}
fn kind_node() -> KindSpec {
    KindSpec {
        runtime: RuntimeKind::Node,
        version_args: &["--version"],
        companion_names: &["npm", "npx"],
    }
}
fn kind_java() -> KindSpec {
    KindSpec {
        runtime: RuntimeKind::Java,
        version_args: &["-version"],
        companion_names: &["javac"],
    }
}
fn kind_rust() -> KindSpec {
    KindSpec {
        runtime: RuntimeKind::Rust,
        version_args: &["--version"],
        companion_names: &["cargo"],
    }
}
fn kind_go() -> KindSpec {
    KindSpec {
        runtime: RuntimeKind::Go,
        version_args: &["version"],
        companion_names: &[],
    }
}

fn probe_candidates(
    spec: KindSpec,
    candidates: Vec<Candidate>,
    warnings: &mut Vec<String>,
) -> Option<HostRuntime> {
    let mut best: Option<HostRuntime> = None;
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        let key = candidate.program.display().to_string().to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        if !candidate.program.is_file() {
            continue;
        }
        let mut program = candidate.program.clone();
        let mut args = candidate.prefix.clone();
        if spec.runtime == RuntimeKind::Python {
            if let Some(real) = python_real_path(&program, &args) {
                program = real;
                args.clear();
            }
        }
        let mut run_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_args.extend(spec.version_args.iter().copied());
        let outcome = process::run_capture(
            &program.display().to_string(),
            &run_args,
            &RunOptions::default().timeout(Duration::from_secs(12)),
        );
        if !outcome.success || outcome.timed_out {
            if let Some(err) = &outcome.spawn_error {
                if warnings.len() < 12 {
                    warnings.push(format!(
                        "Could not probe {} at {}: {err}",
                        spec.runtime.display_name(),
                        program.display()
                    ));
                }
            }
            continue;
        }
        let output = outcome.combined();
        let version = if spec.runtime == RuntimeKind::Java {
            Version::parse_java(&output)
        } else {
            Version::parse(&output)
        };
        let Some(version) = version else {
            if warnings.len() < 12 {
                warnings.push(format!(
                    "Could not understand the {} version banner: {}",
                    spec.runtime.display_name(),
                    output.lines().next().unwrap_or("")
                ));
            }
            continue;
        };
        let mut companions = BTreeMap::new();
        let parent = program.parent().unwrap_or(Path::new("."));
        for name in spec.companion_names {
            let local = parent.join(name);
            if local.is_file() {
                companions.insert((*name).to_string(), local);
            } else if let Some(found) = process::which(name) {
                companions.insert((*name).to_string(), found);
            }
        }
        let runtime = HostRuntime {
            runtime: spec.runtime,
            version,
            executable: program.clone(),
            version_output: output.lines().next().unwrap_or("").trim().to_string(),
            source: candidate.source,
            companions,
        };
        if best
            .as_ref()
            .map(|current| runtime.version > current.version)
            .unwrap_or(true)
        {
            best = Some(runtime);
        }
    }
    best
}

fn python_real_path(program: &Path, prefix: &[String]) -> Option<PathBuf> {
    let mut args: Vec<String> = prefix.to_vec();
    args.push("-c".to_string());
    args.push("import sys; print(sys.executable)".to_string());
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let outcome = process::run_capture(
        &program.display().to_string(),
        &refs,
        &RunOptions::default().timeout(Duration::from_secs(12)),
    );
    if !outcome.success {
        return None;
    }
    let path = outcome.first_line();
    let path = PathBuf::from(path.trim_matches('"'));
    path.is_file().then_some(path)
}

pub fn probe_caches() -> Vec<CacheInfo> {
    let mut caches = Vec::new();
    let mut push = |name: &str, path: PathBuf| {
        caches.push(CacheInfo {
            name: name.to_string(),
            present: path.exists(),
            path,
        });
    };
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache")));
    if let Some(cache) = &cache_home {
        push("pip", cache.join("pip"));
        push("uv", cache.join("uv"));
    }
    if let Some(home) = home_dir() {
        push("npm", home.join(".npm"));
        push("maven", home.join(".m2").join("repository"));
        push("gradle", home.join(".gradle").join("caches"));
        push("cargo", home.join(".cargo").join("registry"));
    }
    caches
}

pub fn top_memory_consumers(limit: usize) -> Vec<ProcessMemory> {
    let mut processes = Vec::new();
    for entry in linuxapi::process_table() {
        let Some(working_set_bytes) = process_rss_bytes(entry.pid) else {
            continue;
        };
        processes.push(ProcessMemory {
            name: entry.name,
            pid: entry.pid,
            working_set_bytes,
        });
    }
    processes.sort_by(|a, b| b.working_set_bytes.cmp(&a.working_set_bytes));
    processes.truncate(limit);
    processes
}

fn process_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_vm_rss(&status)
}

fn parse_vm_rss(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kib.saturating_mul(1024));
        }
    }
    None
}

fn build_profile(
    available: u64,
    load_percent: u32,
    warnings: &mut Vec<String>,
) -> ExecutionProfile {
    let mut profile = ExecutionProfile::default();
    if available == 0 {
        profile
            .advisory
            .push("Memory telemetry unavailable; using normal build parallelism.".to_string());
        return profile;
    }
    let available_mb = available / (1024 * 1024);
    if available < 2 * 1024 * 1024 * 1024 || load_percent >= 85 {
        profile.low_footprint = true;
        profile.parallel_jobs = 1;
        let cap = (available_mb as f64 * 0.6) as u64;
        profile.memory_limit_mb = Some(cap.clamp(512, 4096));
        profile.advisory.push(format!(
            "Low free RAM detected ({:.1} GB available; 2.0 GB recommended). Paging-safe mode enabled.",
            available as f64 / (1024.0 * 1024.0 * 1024.0)
        ));
        warnings.push(
            "Low-memory profile selected automatically. Only sandbox processes are constrained."
                .to_string(),
        );
    }
    profile
}

pub fn match_plan(plan: &ResolutionPlan, report: &ProbeReport) -> Vec<HostMatch> {
    let mut matches = Vec::new();
    let mut runtimes: BTreeSet<RuntimeKind> = plan
        .languages
        .iter()
        .filter_map(|language| language.runtime())
        .collect();
    runtimes.extend(
        plan.requirements
            .iter()
            .map(|requirement| requirement.runtime),
    );
    for runtime in runtimes {
        let requirement = plan.requirement_for(runtime);
        let host = report.runtime(runtime).cloned();
        let status = match (&host, requirement) {
            (Some(host), Some(requirement)) => {
                if requirement.constraint.satisfies(host.version) {
                    MatchStatus::Compatible
                } else {
                    MatchStatus::TooOld
                }
            }
            (Some(_), None) => MatchStatus::NotRequired,
            (None, Some(_)) => MatchStatus::NoHostRuntime,
            (None, None) => MatchStatus::NotRequired,
        };
        let detail = match (&host, requirement, status) {
            (Some(host), Some(requirement), MatchStatus::Compatible) => format!(
                "Host {} {} satisfies {} ({}).",
                runtime.display_name(),
                host.version,
                requirement.constraint,
                requirement.reason
            ),
            (Some(host), Some(requirement), MatchStatus::TooOld) => format!(
                "Host {} {} does not satisfy {} ({}); a minimal runtime will be provisioned.",
                runtime.display_name(),
                host.version,
                requirement.constraint,
                requirement.reason
            ),
            (None, Some(requirement), _) => format!(
                "No {} found; {} requires {} ({}).",
                runtime.display_name(),
                runtime.display_name(),
                requirement.constraint,
                requirement.reason
            ),
            (Some(host), None, _) => format!(
                "Host {} {} found; no version constraint was required.",
                runtime.display_name(),
                host.version
            ),
            _ => format!(
                "{} is not required by this project.",
                runtime.display_name()
            ),
        };
        matches.push(HostMatch {
            runtime,
            requirement: requirement.map(|r| r.constraint.to_string()),
            minimum: requirement.and_then(|r| r.minimum),
            host,
            status,
            detail,
        });
    }
    matches
}

/// Find sandbox metadata left behind by a crash. This function never kills
/// anything; the caller decides with the user whether to clean it.
pub fn find_stale_sandboxes() -> Vec<StaleSandbox> {
    let mut stale = Vec::new();
    let dir = paths::sandboxes_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return stale;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let meta_path = path.join("sandbox.json");
        let Ok(text) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let (id, pid, running) = if let Ok(record) =
            serde_json::from_str::<linuxorchestrator_core::SandboxRecord>(&text)
        {
            (
                record.id,
                record.pid,
                record.lifecycle == linuxorchestrator_core::LifecycleState::Running,
            )
        } else if let Ok(meta) = serde_json::from_str::<SandboxMeta>(&text) {
            (meta.id, meta.pid, meta.status == "running")
        } else {
            continue;
        };
        let alive = pid.map(linuxapi::is_pid_alive).unwrap_or(false);
        if running && !alive {
            stale.push(StaleSandbox {
                id,
                path,
                pid,
                reason: "The orchestrator that owned this sandbox is no longer running."
                    .to_string(),
            });
        }
    }
    stale.sort_by(|a, b| a.id.cmp(&b.id));
    stale
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SandboxMeta {
    id: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    status: String,
}

/// Return the process descendants of a PID using the read-only `/proc` table.
pub fn descendants(pid: u32) -> Vec<u32> {
    linuxapi::process_tree(pid)
}

pub fn process_entries() -> Vec<ProcessEntry> {
    linuxapi::process_table()
}

pub fn human_mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vm_rss() {
        let status = "Name:\tpython3\nVmRSS:\t  123456 kB\n";
        assert_eq!(parse_vm_rss(status), Some(123_456 * 1024));
    }

    #[test]
    fn matches_required_runtime_against_host() {
        let plan = ResolutionPlan::new(PathBuf::from("/work/demo"), "demo".to_string());
        let report = ProbeReport::default();
        let matches = match_plan(&plan, &report);
        assert!(matches.is_empty());
    }
}
