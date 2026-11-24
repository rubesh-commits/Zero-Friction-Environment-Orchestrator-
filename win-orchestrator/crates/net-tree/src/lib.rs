use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;
use winorchestrator_core::process::{self, RunOptions};
use winorchestrator_core::{paths, winapi};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    Serving,
    Receiving,
    Internal,
    Unknown,
}

impl FlowRole {
    pub fn label(self) -> &'static str {
        match self {
            FlowRole::Serving => "SERVING",
            FlowRole::Receiving => "RECEIVING",
            FlowRole::Internal => "INTERNAL",
            FlowRole::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub protocol: String,
    pub local_address: String,
    pub local_port: Option<u16>,
    pub remote_address: String,
    pub remote_port: Option<u16>,
    pub state: String,
    pub pid: u32,
    pub process_name: String,
    pub role: FlowRole,
}

impl Connection {
    pub fn local_endpoint(&self) -> String {
        endpoint(&self.local_address, self.local_port)
    }

    pub fn remote_endpoint(&self) -> String {
        endpoint(&self.remote_address, self.remote_port)
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {:<9} {} -> {} ({})",
            self.protocol,
            self.role.label(),
            self.local_endpoint(),
            self.remote_endpoint(),
            if self.state.is_empty() {
                "-"
            } else {
                &self.state
            }
        )
    }
}

fn endpoint(address: &str, port: Option<u16>) -> String {
    match port {
        Some(port) if address.contains(':') => format!("[{address}]:{port}"),
        Some(port) => format!("{address}:{port}"),
        None => format!("{address}:*"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessNode {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub depth: usize,
    pub is_root: bool,
    pub connections: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetTreeReport {
    pub generated_at: String,
    pub root_pids: Vec<u32>,
    pub nodes: Vec<ProcessNode>,
    pub connections: Vec<Connection>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl NetTreeReport {
    pub fn serving_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|connection| connection.role == FlowRole::Serving)
            .count()
    }

    pub fn receiving_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|connection| connection.role == FlowRole::Receiving)
            .count()
    }

    pub fn internal_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|connection| connection.role == FlowRole::Internal)
            .count()
    }

    pub fn render_tree(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "Process/socket tree for {} pid(s): {} node(s), {} serving, {} receiving, {} internal",
            self.root_pids.len(),
            self.nodes.len(),
            self.serving_count(),
            self.receiving_count(),
            self.internal_count()
        ));
        let mut by_parent: HashMap<u32, Vec<&ProcessNode>> = HashMap::new();
        for node in &self.nodes {
            by_parent.entry(node.parent_pid).or_default().push(node);
        }
        for children in by_parent.values_mut() {
            children.sort_by_key(|node| (node.name.to_ascii_lowercase(), node.pid));
        }
        let mut by_pid: HashMap<u32, Vec<&Connection>> = HashMap::new();
        for connection in &self.connections {
            by_pid.entry(connection.pid).or_default().push(connection);
        }
        for connections in by_pid.values_mut() {
            connections.sort_by_key(|connection| {
                (
                    connection.local_port.unwrap_or(0),
                    connection.remote_endpoint(),
                )
            });
        }
        let root_set: BTreeSet<u32> = self.root_pids.iter().copied().collect();
        let roots: Vec<&ProcessNode> = self
            .nodes
            .iter()
            .filter(|node| root_set.contains(&node.pid) || node.is_root)
            .collect();
        let mut visited = BTreeSet::new();
        for root in roots {
            render_node(root, 0, &by_parent, &by_pid, &mut visited, &mut lines);
        }
        // Any orphan that lost its parent link is still shown rather than hidden.
        for node in &self.nodes {
            if !visited.contains(&node.pid) {
                render_node(node, 0, &by_parent, &by_pid, &mut visited, &mut lines);
            }
        }
        lines.join("\n")
    }

    pub fn render_connections(&self) -> String {
        let mut rows = Vec::new();
        rows.push(format!(
            "{:<6} {:<10} {:<28} {:<28} {:<12} {:>7}  {}",
            "PROTO", "ROLE", "LOCAL", "REMOTE", "STATE", "PID", "PROCESS"
        ));
        for connection in &self.connections {
            rows.push(format!(
                "{:<6} {:<10} {:<28} {:<28} {:<12} {:>7}  {}",
                connection.protocol,
                connection.role.label(),
                connection.local_endpoint(),
                connection.remote_endpoint(),
                if connection.state.is_empty() {
                    "-"
                } else {
                    &connection.state
                },
                connection.pid,
                connection.process_name
            ));
        }
        if self.connections.is_empty() {
            rows.push("(no matching sockets)".to_string());
        }
        rows.join("\n")
    }
}

fn render_node(
    node: &ProcessNode,
    indent: usize,
    by_parent: &HashMap<u32, Vec<&ProcessNode>>,
    by_pid: &HashMap<u32, Vec<&Connection>>,
    visited: &mut BTreeSet<u32>,
    lines: &mut Vec<String>,
) {
    if !visited.insert(node.pid) {
        return;
    }
    let pad = "  ".repeat(indent);
    let marker = if node.is_root { "*" } else { "-" };
    lines.push(format!(
        "{pad}{marker} [{}] {} (parent {}, {} socket(s))",
        node.pid, node.name, node.parent_pid, node.connections
    ));
    if let Some(connections) = by_pid.get(&node.pid) {
        for connection in connections {
            lines.push(format!("{pad}    {}", connection.describe()));
        }
    }
    if let Some(children) = by_parent.get(&node.pid) {
        for child in children {
            if child.pid != node.pid {
                render_node(child, indent + 1, by_parent, by_pid, visited, lines);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawConnection {
    pub protocol: String,
    pub local: String,
    pub remote: String,
    pub state: String,
    pub pid: u32,
}

/// Parse `netstat -ano` output. Public for deterministic tests.
pub fn parse_netstat(text: &str) -> Vec<RawConnection> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            continue;
        }
        let protocol = tokens[0].to_ascii_lowercase();
        if protocol != "tcp" && protocol != "udp" {
            continue;
        }
        let local = tokens[1].to_string();
        let remote = tokens[2].to_string();
        let (state, pid_text) = if protocol == "tcp" && tokens.len() >= 5 {
            (tokens[3].to_string(), tokens[4])
        } else if tokens.len() >= 4 {
            (String::new(), tokens[3])
        } else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        rows.push(RawConnection {
            protocol,
            local,
            remote,
            state,
            pid,
        });
    }
    rows
}

fn split_endpoint(text: &str) -> (String, Option<u16>) {
    let text = text.trim();
    if text == "*:*" || text == "*" {
        return ("*".to_string(), None);
    }
    if let Some(rest) = text.strip_prefix('[') {
        if let Some((address, port)) = rest.split_once("]:") {
            return (address.to_string(), port.parse::<u16>().ok());
        }
    }
    match text.rsplit_once(':') {
        Some((address, port)) => (address.to_string(), port.parse::<u16>().ok()),
        None => (text.to_string(), None),
    }
}

fn is_loopback(address: &str) -> bool {
    let address = address.trim_matches(['[', ']']);
    address == "127.0.0.1"
        || address == "::1"
        || address == "0.0.0.0"
        || address == "::"
        || address == "*"
        || address.starts_with("127.")
}

fn classify(raw: &RawConnection, local_port: Option<u16>, remote_port: Option<u16>) -> FlowRole {
    let state = raw.state.to_ascii_uppercase();
    if state == "LISTENING" {
        return FlowRole::Serving;
    }
    if raw.protocol == "udp" && raw.remote == "*:*" {
        return FlowRole::Serving;
    }
    if local_port.is_some() && remote_port.is_none() {
        return FlowRole::Serving;
    }
    if is_loopback(&raw.local) && is_loopback(&raw.remote) {
        return FlowRole::Internal;
    }
    if remote_port.is_some() {
        FlowRole::Receiving
    } else {
        FlowRole::Unknown
    }
}

pub fn collect(root_pids: &[u32]) -> NetTreeReport {
    let table = winapi::process_table();
    let netstat = process::run_capture(
        "netstat.exe",
        &["-ano"],
        &RunOptions::default().timeout(Duration::from_secs(20)),
    );
    let text = netstat.stdout.clone();
    let mut report = report_from(root_pids, &table, &text);
    if !netstat.success && netstat.spawn_error.is_none() {
        report.warnings.push(format!(
            "netstat.exe returned a failure: {}",
            netstat.combined()
        ));
    } else if netstat.spawn_error.is_some() {
        report.warnings.push(format!(
            "could not run netstat.exe: {}",
            netstat.spawn_error.unwrap_or_default()
        ));
    }
    report
}

pub fn collect_all() -> NetTreeReport {
    let table = winapi::process_table();
    let roots: Vec<u32> = table.iter().map(|entry| entry.pid).collect();
    let netstat = process::run_capture(
        "netstat.exe",
        &["-ano"],
        &RunOptions::default().timeout(Duration::from_secs(20)),
    );
    let mut report = report_from(&roots, &table, &netstat.stdout);
    if !netstat.success && netstat.spawn_error.is_some() {
        report.warnings.push(format!(
            "could not run netstat.exe: {}",
            netstat.spawn_error.unwrap_or_default()
        ));
    }
    report
}

/// Build a report from an explicit process table and netstat text. This is the
/// deterministic core used by the live observer and by tests.
pub fn report_from(
    root_pids: &[u32],
    table: &[winapi::ProcessEntry],
    netstat_text: &str,
) -> NetTreeReport {
    let mut report = NetTreeReport {
        generated_at: paths::iso8601_now(),
        root_pids: root_pids.to_vec(),
        ..Default::default()
    };
    let names: HashMap<u32, String> = table
        .iter()
        .map(|entry| (entry.pid, entry.name.clone()))
        .collect();
    let parents: HashMap<u32, u32> = table
        .iter()
        .map(|entry| (entry.pid, entry.parent_pid))
        .collect();

    let mut included = BTreeSet::new();
    if root_pids.is_empty() {
        included.extend(table.iter().map(|entry| entry.pid));
    } else {
        for root in root_pids {
            if !names.contains_key(root) {
                report.warnings.push(format!(
                    "pid {root} is no longer running; its sockets cannot be attributed."
                ));
                continue;
            }
            included.extend(tree_from_table(*root, table));
        }
    }

    let mut connection_counts: HashMap<u32, usize> = HashMap::new();
    for raw in parse_netstat(netstat_text) {
        if !included.contains(&raw.pid) {
            continue;
        }
        let (local_address, local_port) = split_endpoint(&raw.local);
        let (remote_address, remote_port) = split_endpoint(&raw.remote);
        let role = classify(&raw, local_port, remote_port);
        *connection_counts.entry(raw.pid).or_default() += 1;
        report.connections.push(Connection {
            protocol: raw.protocol,
            local_address,
            local_port,
            remote_address,
            remote_port,
            state: raw.state,
            pid: raw.pid,
            process_name: names.get(&raw.pid).cloned().unwrap_or_default(),
            role,
        });
    }
    report.connections.sort_by_key(|connection| {
        (
            connection.pid,
            connection.local_port.unwrap_or(0),
            connection.remote_endpoint(),
        )
    });

    for pid in &included {
        let Some(name) = names.get(pid) else {
            continue;
        };
        let parent_pid = parents.get(pid).copied().unwrap_or(0);
        let mut depth = 0usize;
        let mut cursor = *pid;
        let mut guard = 0;
        while depth < 128 {
            let Some(parent) = parents.get(&cursor).copied() else {
                break;
            };
            if parent == 0 || parent == cursor || !included.contains(&parent) {
                break;
            }
            depth += 1;
            cursor = parent;
            guard += 1;
            if guard > 256 {
                break;
            }
        }
        report.nodes.push(ProcessNode {
            pid: *pid,
            parent_pid,
            name: name.clone(),
            depth,
            is_root: root_pids.contains(pid),
            connections: connection_counts.get(pid).copied().unwrap_or(0),
        });
    }
    report
        .nodes
        .sort_by_key(|node| (node.depth, node.name.to_ascii_lowercase(), node.pid));
    report
}

fn tree_from_table(root: u32, table: &[winapi::ProcessEntry]) -> Vec<u32> {
    let mut result = vec![root];
    let mut index = 0;
    while index < result.len() {
        let current = result[index];
        for entry in table {
            if entry.parent_pid == current && !result.contains(&entry.pid) {
                result.push(entry.pid);
            }
        }
        index += 1;
    }
    result.sort_unstable();
    result
}

/// Convenience for port-conflict warnings before a sandbox starts.
pub fn listening_ports() -> BTreeMap<u16, Vec<u32>> {
    let report = collect_all();
    let mut ports: BTreeMap<u16, Vec<u32>> = BTreeMap::new();
    for connection in report.connections {
        if connection.role == FlowRole::Serving {
            if let Some(port) = connection.local_port {
                ports.entry(port).or_default().push(connection.pid);
            }
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    use winorchestrator_core::winapi;

    #[test]
    fn parses_tcp_and_udp_rows() {
        let text = "\
  TCP    0.0.0.0:8000     0.0.0.0:0      LISTENING     4321
  TCP    127.0.0.1:8000   127.0.0.1:55000 ESTABLISHED   4321
  TCP    [::]:5432        [::]:0         LISTENING     999
  UDP    0.0.0.0:5353     *:*                           1111
  TCP    1.2.3.4:5000     5.6.7.8:443    ESTABLISHED   2222
";
        let rows = parse_netstat(text);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].pid, 4321);
        assert_eq!(rows[0].state, "LISTENING");
        assert_eq!(rows[2].local, "[::]:5432");
        assert_eq!(rows[3].protocol, "udp");
        assert_eq!(rows[3].pid, 1111);
    }

    #[test]
    fn classifies_serving_receiving_and_internal() {
        let table = vec![
            winapi::ProcessEntry {
                pid: 100,
                parent_pid: 0,
                name: "server.exe".to_string(),
            },
            winapi::ProcessEntry {
                pid: 200,
                parent_pid: 100,
                name: "client.exe".to_string(),
            },
        ];
        let text = "\
  TCP    0.0.0.0:8080     0.0.0.0:0      LISTENING     100
  TCP    127.0.0.1:8080   127.0.0.1:52000 ESTABLISHED  100
  TCP    127.0.0.1:52000  127.0.0.1:8080 ESTABLISHED  200
  TCP    10.0.0.2:50100   93.184.216.34:443 ESTABLISHED 200
";
        let report = report_from(&[100], &table, text);
        assert_eq!(report.nodes.len(), 2);
        assert_eq!(report.connections.len(), 4);
        assert_eq!(report.serving_count(), 1);
        assert_eq!(report.receiving_count(), 1);
        assert_eq!(report.internal_count(), 2);
        let tree = report.render_tree();
        assert!(tree.contains("server.exe"));
        assert!(tree.contains("client.exe"));
        assert!(tree.contains("SERVING"));
    }

    #[test]
    fn splits_ipv6_and_wildcards() {
        assert_eq!(split_endpoint("[::]:443"), ("::".to_string(), Some(443)));
        assert_eq!(split_endpoint("*:*"), ("*".to_string(), None));
        assert_eq!(
            split_endpoint("127.0.0.1:8000"),
            ("127.0.0.1".to_string(), Some(8000))
        );
    }

    #[test]
    fn reports_missing_pids_instead_of_crashing() {
        let table = vec![winapi::ProcessEntry {
            pid: 1,
            parent_pid: 0,
            name: "x.exe".to_string(),
        }];
        let report = report_from(&[999], &table, "");
        assert!(report.nodes.is_empty());
        assert!(!report.warnings.is_empty());
    }
}
