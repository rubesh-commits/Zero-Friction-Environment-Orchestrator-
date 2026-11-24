use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStatus {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub load_percent: u32,
}

impl MemoryStatus {
    pub fn used_bytes(&self) -> u64 { self.total_bytes.saturating_sub(self.available_bytes) }
    pub fn available_gib(&self) -> f64 { self.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0) }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
}

pub fn memory_status() -> Option<MemoryStatus> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0;
    let mut avail = 0;

    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            total = line.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024;
        } else if line.starts_with("MemAvailable:") {
            avail = line.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024;
        }
    }

    if total > 0 {
        let used = total.saturating_sub(avail);
        let percent = ((used as f64 / total as f64) * 100.0) as u32;
        Some(MemoryStatus { total_bytes: total, available_bytes: avail, load_percent: percent })
    } else {
        None
    }
}

/// Read free space for the filesystem containing `path` using the POSIX
/// `df -P -k` output, which is portable across Linux distributions.
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    let output = std::process::Command::new("df")
        .arg("-P")
        .arg("-k")
        .arg("--")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }
    let total_kib = fields[1].parse::<u64>().ok()?;
    let available_kib = fields[3].parse::<u64>().ok()?;
    Some(DiskSpace {
        total_bytes: total_kib.saturating_mul(1024),
        free_bytes: available_kib.saturating_mul(1024),
        available_bytes: available_kib.saturating_mul(1024),
    })
}

/// Snapshot of `/proc` used for read-only process introspection.
pub fn process_table() -> Vec<ProcessEntry> {
    let mut entries = Vec::new();
    let Ok(dir) = fs::read_dir("/proc") else {
        return entries;
    };
    for entry in dir.filter_map(|entry| entry.ok()) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(open) = stat.find('(') else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let name = stat[open + 1..close].to_string();
        let mut fields = stat[close + 1..].split_whitespace();
        let _state = fields.next();
        let Some(parent_pid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        entries.push(ProcessEntry {
            pid,
            parent_pid,
            name,
        });
    }
    entries.sort_by_key(|entry| entry.pid);
    entries
}

pub fn is_pid_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{}", pid)).exists()
}

/// Return ISO 8601 formatted timestamp.
pub fn iso8601_now() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{stamp}")
}

/// Return `pid` plus every descendant, using `/proc` parent links. Result is
/// sorted and deduplicated.
pub fn process_tree(root_pid: u32) -> Vec<u32> {
    let table = process_table();
    let mut result = vec![root_pid];
    let mut index = 0;
    while index < result.len() {
        let current = result[index];
        for entry in &table {
            if entry.parent_pid == current && !result.contains(&entry.pid) {
                result.push(entry.pid);
            }
        }
        index += 1;
    }
    result.sort_unstable();
    result
}

/// Linux has no Job Object equivalent that can adopt an already-running
/// process. The caller still runs sandboxed commands; they simply are not
/// hard-capped by this handle.
pub struct JobHandle {
    _private: (),
}

impl JobHandle {
    pub fn create(_memory_limit_mb: Option<u64>) -> Option<Self> {
        None
    }

    pub fn assign_pid(&self, _pid: u32) -> bool {
        false
    }

    pub fn terminate(&self) -> bool {
        false
    }
}

/// Resume a stopped process with SIGCONT.
pub fn resume_process(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-CONT")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
