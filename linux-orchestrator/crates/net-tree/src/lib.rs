#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
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

/// Raw connection read from procfs net output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawConnection {
    pub protocol: String,
    pub local: String,
    pub remote: String,
    pub state: String,
    pub pid: u32,
}

/// Process entry read from /proc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
}

/// Parsed `netstat -ano` output.
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
        let pid_str = if protocol == "tcp" && tokens.len() >= 5 {
            tokens[4].to_string()
        } else if tokens.len() >= 4 {
            tokens[3].to_string()
        } else {
            continue;
        };
        let state = if protocol == "tcp" && tokens.len() >= 5 {
            tokens[3].to_string()
        } else {
            String::new()
        };
        let pid = match pid_str.parse::<u32>() {
            Ok(p) if p != 0 => p,
            _ => continue,
        };
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

/// Split an endpoint string like "127.0.0.1:8000" or "[::]:443" into (ip, port).
pub fn split_endpoint(ep: &str) -> (String, Option<u16>) {
    let ep = ep.trim();
    if ep.starts_with('[') {
        let end = ep.find(']').unwrap();
        let ip = &ep[1..end];
        let rest = &ep[end + 1..];
        let port: Option<u16> = rest.strip_prefix(':').and_then(|s| s.parse().ok());
        return (ip.to_string(), port);
    }
    if ep.contains(':') {
        let parts: Vec<&str> = ep.splitn(2, ':').collect();
        let ip = parts[0].to_string();
        let port: Option<u16> = parts[1].parse().ok();
        return (ip, port);
    }
    (ep.to_string(), None)
}

/// Report from netstat text and process table.
pub fn report_from(pids: &[u32], table: &[ProcessEntry], text: &str) -> NetTreeReport {
    let mut nodes = Vec::new();
    let mut connections = Vec::new();
    let mut warnings = Vec::new();

    let rows = parse_netstat(text);
    let pid_set: std::collections::HashSet<u32> = pids.iter().copied().collect();
    // Include only connections whose pid is in the requested set
    for row in &rows {
        let pid = row.pid;
        if !pid_set.contains(&pid) {
            continue;
        }
        connections.push(Connection {
            local: row.local.clone(),
            remote: row.remote.clone(),
            protocol: row.protocol.clone(),
            state: row.state.clone(),
            pid,
        });
        nodes.push(Node::Connection {
            label: format!("pid{}", pid),
            pid,
        });
    }

    // Process table entries: include those whose pid is in the requested set
    let mut table_seen = std::collections::HashSet::new();
    for entry in table {
        if pid_set.contains(&entry.pid) && !table_seen.contains(&entry.pid) {
            table_seen.insert(entry.pid);
            nodes.push(Node::Process {
                label: entry.name.clone(),
                pid: entry.pid,
            });
        }
    }

    // Count classifications
    let serving_count = connections
        .iter()
        .filter(|c| c.state.to_uppercase() == "LISTENING")
        .count();
    let receiving_count = connections
        .iter()
        .filter(|c| c.state.to_uppercase().contains("ESTABLISHED"))
        .count();
    let internal_count = connections
        .iter()
        .filter(|c| {
            c.local.contains("127.0.0.1") || c.remote.contains("127.0.0.1")
        })
        .count();

    // Warnings for requested pids not found in connections
    let connected_pids: std::collections::HashSet<u32> = connections.iter().map(|c| c.pid).collect();
    for &pid in &pid_set {
        if !connected_pids.contains(&pid) {
            warnings.push(format!("pid {} not found in netstat output", pid));
        }
    }

    NetTreeReport {
        nodes,
        connections,
        warnings,
        serving_count,
        receiving_count,
        internal_count,
    }
}

/// Tree from table.
pub fn tree_from_table(report: &NetTreeReport) -> String {
    report.render_tree()
}

/// Collect from text.
pub fn collect_from_text(text: &str) -> NetTreeReport {
    report_from(&[], &[], text)
}

/// Listen ports.
pub fn listening_ports(text: &str) -> Vec<RawConnection> {
    parse_netstat(text)
        .into_iter()
        .filter(|c| c.state.to_uppercase() == "LISTENING")
        .collect()
}

/// Collect.
pub fn collect(root_pids: &[u32]) -> NetTreeReport {
    report_from(root_pids, &[], "")
}

/// Netstat text helper.
pub fn netstat_text() -> String {
    String::new()
}

/// Endpoint helper.
pub fn endpoint(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Process { label: String, pid: u32 },
    Connection { label: String, pid: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub local: String,
    pub remote: String,
    pub protocol: String,
    pub state: String,
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetTreeReport {
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
    pub warnings: Vec<String>,
    pub serving_count: usize,
    pub receiving_count: usize,
    pub internal_count: usize,
}

impl NetTreeReport {
    pub fn render_tree(&self) -> String {
        let mut result = String::new();
        for node in &self.nodes {
            match node {
                Node::Process { label, .. } => result.push_str(&format!("{}\n", label)),
                Node::Connection { label, .. } => result.push_str(&format!("{}\n", label)),
            }
        }
        for conn in &self.connections {
            result.push_str(&format!("  {} -> {}\n", conn.local, conn.remote));
        }
        result
    }

    pub fn serving_count(&self) -> usize {
        self.serving_count
    }

    pub fn receiving_count(&self) -> usize {
        self.receiving_count
    }

    pub fn internal_count(&self) -> usize {
        self.internal_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ProcessEntry {
                pid: 100,
                parent_pid: 0,
                name: "server".to_string(),
            },
            ProcessEntry {
                pid: 200,
                parent_pid: 100,
                name: "client".to_string(),
            },
        ];
        let text = "\
  TCP    0.0.0.0:8080     0.0.0.0:0      LISTENING     100
  TCP    127.0.0.1:8080   127.0.0.1:52000 ESTABLISHED  100
  TCP    127.0.0.1:52000  127.0.0.1:8080 ESTABLISHED  200
  TCP    10.0.0.2:50100   93.184.216.34:443 ESTABLISHED 200
";
        let report = report_from(&[100], &table, text);
        assert_eq!(report.nodes.len(), 3);
        assert_eq!(report.connections.len(), 2);
        assert_eq!(report.serving_count(), 1);
        assert_eq!(report.receiving_count(), 1);
        assert_eq!(report.internal_count(), 1);
        let tree = report.render_tree();
        assert!(tree.contains("server"));
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
        let table = vec![ProcessEntry {
            pid: 1,
            parent_pid: 0,
            name: "x".to_string(),
        }];
        let report = report_from(&[999], &table, "");
        assert!(report.nodes.is_empty());
        assert!(!report.warnings.is_empty());
    }
}