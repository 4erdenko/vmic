use anyhow::{Context as _, Result};
use once_cell::sync::Lazy;
use procfs::net::{self, TcpState};
use procfs::process;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

const MAX_SOCKET_SAMPLES: usize = 20;

struct NetworkCollector;

impl Collector for NetworkCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "network",
            title: "Network Overview",
            description: "Interfaces and listening sockets",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match build_snapshot() {
            Ok((snapshot, notes)) => {
                let summary = format!(
                    "{} interfaces, {} listening sockets",
                    snapshot.interfaces.len(),
                    snapshot.listeners.counts.total()
                );

                let body = json!({
                    "interfaces": snapshot.interfaces,
                    "listeners": {
                        "counts": snapshot.listeners.counts,
                        "samples": snapshot.listeners.samples,
                        "groups": snapshot.listeners.groups,
                        "insights": snapshot.listeners.insights,
                    }
                });

                let mut section = Section::success("network", "Network Overview", body);
                section.summary = Some(summary);
                section.notes.extend(notes);
                Ok(section)
            }
            Err(err) => Ok(Section::degraded(
                "network",
                "Network Overview",
                err.to_string(),
                json!({
                    "interfaces": [],
                    "listeners": {
                        "counts": ListenerCounts::default(),
                        "samples": Vec::<serde_json::Value>::new(),
                        "groups": Vec::<serde_json::Value>::new(),
                        "insights": Vec::<serde_json::Value>::new(),
                    }
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(NetworkCollector)
}

register_collector!(create_collector);

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct InterfaceInfo {
    name: String,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_packets: u64,
    tx_packets: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq, Default)]
struct ListenerCounts {
    tcp: usize,
    tcp6: usize,
    udp: usize,
    udp6: usize,
}

impl ListenerCounts {
    fn total(&self) -> usize {
        self.tcp + self.tcp6 + self.udp + self.udp6
    }
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct SocketSample {
    protocol: String,
    local_address: String,
    state: Option<String>,
    processes: Vec<SocketProcessInfo>,
    service: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct SocketProcessInfo {
    pid: i32,
    command: String,
    uid: u32,
    container: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ListenerSnapshot {
    counts: ListenerCounts,
    samples: Vec<SocketSample>,
    groups: Vec<ListenerContainerGroup>,
    insights: Vec<ListenerInsight>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct NetworkSnapshot {
    interfaces: Vec<InterfaceInfo>,
    listeners: ListenerSnapshot,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ListenerContainerGroup {
    container: Option<String>,
    socket_count: usize,
    process_count: usize,
    processes: Vec<ListenerProcessGroup>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ListenerProcessGroup {
    pid: i32,
    command: String,
    uid: u32,
    socket_count: usize,
    protocols: Vec<String>,
    local_addresses: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ListenerInsight {
    rule: String,
    severity: String,
    message: String,
    sockets: Vec<SocketReference>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct SocketReference {
    protocol: String,
    local_address: String,
    service: Option<String>,
    container: Option<String>,
    pid: Option<i32>,
}

fn build_snapshot() -> Result<(NetworkSnapshot, Vec<String>)> {
    let interfaces = gather_interfaces().context("failed to read network interfaces")?;

    if interfaces.is_empty() {
        anyhow::bail!("no network interface data available")
    }

    let (listeners, notes) = gather_listeners();

    Ok((
        NetworkSnapshot {
            interfaces,
            listeners,
        },
        notes,
    ))
}

fn gather_interfaces() -> Result<Vec<InterfaceInfo>> {
    let stats = net::dev_status()?;
    let mut interfaces: Vec<_> = stats
        .into_iter()
        .map(|(name, device)| InterfaceInfo {
            name,
            rx_bytes: device.recv_bytes,
            tx_bytes: device.sent_bytes,
            rx_packets: device.recv_packets,
            tx_packets: device.sent_packets,
        })
        .collect();

    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(interfaces)
}

fn gather_listeners() -> (ListenerSnapshot, Vec<String>) {
    let mut samples = Vec::new();
    let mut counts = ListenerCounts::default();
    let mut notes = Vec::new();
    let process_map = collect_socket_process_map().unwrap_or_default();

    match net::tcp() {
        Ok(entries) => {
            for entry in entries.into_iter().filter(|e| e.state == TcpState::Listen) {
                counts.tcp += 1;
                if samples.len() < MAX_SOCKET_SAMPLES {
                    let processes = process_map.get(&entry.inode).cloned().unwrap_or_default();
                    let protocol = "tcp".to_string();
                    let local_address = format!("{}", entry.local_address);
                    samples.push(SocketSample {
                        protocol: protocol.clone(),
                        local_address: local_address.clone(),
                        state: Some(format!("{:?}", entry.state)),
                        processes,
                        service: classify_service(&protocol, &local_address),
                    });
                }
            }
        }
        Err(err) => notes.push(format!("Failed to read /proc/net/tcp: {}", err)),
    }

    match net::tcp6() {
        Ok(entries) => {
            for entry in entries.into_iter().filter(|e| e.state == TcpState::Listen) {
                counts.tcp6 += 1;
                if samples.len() < MAX_SOCKET_SAMPLES {
                    let processes = process_map.get(&entry.inode).cloned().unwrap_or_default();
                    let protocol = "tcp6".to_string();
                    let local_address = format!("{}", entry.local_address);
                    samples.push(SocketSample {
                        protocol: protocol.clone(),
                        local_address: local_address.clone(),
                        state: Some(format!("{:?}", entry.state)),
                        processes,
                        service: classify_service(&protocol, &local_address),
                    });
                }
            }
        }
        Err(err) => notes.push(format!("Failed to read /proc/net/tcp6: {}", err)),
    }

    match net::udp() {
        Ok(entries) => {
            counts.udp = entries.len();
            for entry in entries
                .into_iter()
                .take(MAX_SOCKET_SAMPLES.saturating_sub(samples.len()))
            {
                let processes = process_map.get(&entry.inode).cloned().unwrap_or_default();
                let protocol = "udp".to_string();
                let local_address = format!("{}", entry.local_address);
                samples.push(SocketSample {
                    protocol: protocol.clone(),
                    local_address: local_address.clone(),
                    state: None,
                    processes,
                    service: classify_service(&protocol, &local_address),
                });
            }
        }
        Err(err) => notes.push(format!("Failed to read /proc/net/udp: {}", err)),
    }

    match net::udp6() {
        Ok(entries) => {
            counts.udp6 = entries.len();
            for entry in entries
                .into_iter()
                .take(MAX_SOCKET_SAMPLES.saturating_sub(samples.len()))
            {
                let processes = process_map.get(&entry.inode).cloned().unwrap_or_default();
                let protocol = "udp6".to_string();
                let local_address = format!("{}", entry.local_address);
                samples.push(SocketSample {
                    protocol: protocol.clone(),
                    local_address: local_address.clone(),
                    state: None,
                    processes,
                    service: classify_service(&protocol, &local_address),
                });
            }
        }
        Err(err) => notes.push(format!("Failed to read /proc/net/udp6: {}", err)),
    }

    let groups = build_listener_groups(&samples);
    let insights = derive_listener_insights(&samples);

    (
        ListenerSnapshot {
            counts,
            samples,
            groups,
            insights,
        },
        notes,
    )
}

fn collect_socket_process_map() -> Result<HashMap<u64, Vec<SocketProcessInfo>>> {
    let mut map: HashMap<u64, Vec<SocketProcessInfo>> = HashMap::new();
    let processes = process::all_processes()?;

    for proc in processes {
        let proc = match proc {
            Ok(proc) => proc,
            Err(_) => continue,
        };
        let pid = proc.pid();
        let command = proc.stat().map(|s| s.comm).unwrap_or_else(|_| "?".into());
        let uid = proc.uid().unwrap_or(0);
        let container = proc
            .cgroups()
            .ok()
            .and_then(|groups| extract_container_from_cgroups(&groups));

        let processes_entry = SocketProcessInfo {
            pid,
            command,
            uid,
            container,
        };

        if let Ok(fds) = proc.fd() {
            for fd in fds {
                if let Ok(fd) = fd {
                    if let process::FDTarget::Socket(inode) = fd.target {
                        map.entry(inode).or_default().push(processes_entry.clone());
                    }
                }
            }
        }
    }

    Ok(map)
}

fn extract_container_from_cgroups(groups: &procfs::ProcessCGroups) -> Option<String> {
    for group in &groups.0 {
        let path = group.pathname.trim_matches('/');
        if path.contains("docker/") {
            if let Some(id) = path.split("docker/").nth(1) {
                return Some(id.split('/').next().unwrap_or(id).to_string());
            }
        }
        if path.contains("kubepods/") {
            if let Some(id) = path.rsplit('/').next() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn build_listener_groups(samples: &[SocketSample]) -> Vec<ListenerContainerGroup> {
    let mut process_groups: HashMap<i32, ListenerProcessGroupBuilder> = HashMap::new();

    for sample in samples {
        for process in &sample.processes {
            let entry = process_groups
                .entry(process.pid)
                .or_insert_with(|| ListenerProcessGroupBuilder::new(process));
            entry.socket_count = entry.socket_count.saturating_add(1);
            entry.protocols.insert(sample.protocol.clone());
            entry.local_addresses.insert(sample.local_address.clone());
        }
    }

    let mut container_groups: HashMap<Option<String>, ListenerContainerGroupBuilder> =
        HashMap::new();

    for builder in process_groups.into_values() {
        let (container, process_group) = builder.finish();
        let entry = container_groups
            .entry(container.clone())
            .or_insert_with(|| ListenerContainerGroupBuilder::new(container));
        entry.socket_count = entry
            .socket_count
            .saturating_add(process_group.socket_count);
        entry.process_count = entry.process_count.saturating_add(1);
        entry.processes.push(process_group);
    }

    let mut groups: Vec<_> = container_groups
        .into_values()
        .map(|builder| builder.finish())
        .collect();
    groups.sort_by(|a, b| b.socket_count.cmp(&a.socket_count));
    groups
}

fn derive_listener_insights(samples: &[SocketSample]) -> Vec<ListenerInsight> {
    let mut rules: BTreeMap<&'static str, InsightBucket> = BTreeMap::new();

    for sample in samples {
        if is_wildcard_address(&sample.local_address) {
            rules
                .entry("wildcard_listener")
                .or_insert_with(|| {
                    InsightBucket::new("warning", "Listener bound to all interfaces")
                })
                .push(sample);
        }

        if sample
            .service
            .as_deref()
            .map(|service| INSECURE_SERVICES.contains(service))
            .unwrap_or(false)
        {
            rules
                .entry("legacy_protocol")
                .or_insert_with(|| {
                    InsightBucket::new("warning", "Legacy or insecure protocol exposed")
                })
                .push(sample);
        }
    }

    rules
        .into_iter()
        .map(|(rule, bucket)| ListenerInsight {
            rule: rule.to_string(),
            severity: bucket.severity,
            message: bucket.message,
            sockets: bucket.sockets,
        })
        .collect()
}

struct InsightBucket {
    severity: String,
    message: String,
    sockets: Vec<SocketReference>,
}

impl InsightBucket {
    fn new(severity: &str, message: &str) -> Self {
        InsightBucket {
            severity: severity.to_string(),
            message: message.to_string(),
            sockets: Vec::new(),
        }
    }

    fn push(&mut self, sample: &SocketSample) {
        let reference = SocketReference {
            protocol: sample.protocol.clone(),
            local_address: sample.local_address.clone(),
            service: sample.service.clone(),
            container: sample
                .processes
                .iter()
                .find_map(|process| process.container.clone()),
            pid: sample.processes.first().map(|process| process.pid),
        };

        self.sockets.push(reference);
    }
}

fn classify_service(protocol: &str, local_address: &str) -> Option<String> {
    let port = extract_port(local_address)?;
    let key = (protocol.to_ascii_lowercase(), port);
    SERVICE_TABLE.get(&key).cloned()
}

fn extract_port(address: &str) -> Option<u16> {
    address
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
}

fn is_wildcard_address(address: &str) -> bool {
    address.starts_with("0.0.0.0:")
        || address.starts_with(":::")
        || address.starts_with("[::]:")
        || address.starts_with("[::ffff:0.0.0.0]:")
}

static SERVICE_TABLE: Lazy<BTreeMap<(String, u16), String>> = Lazy::new(|| {
    let mut map = BTreeMap::new();
    insert_service(&mut map, "tcp", 21, "ftp");
    insert_service(&mut map, "tcp", 22, "ssh");
    insert_service(&mut map, "tcp", 23, "telnet");
    insert_service(&mut map, "tcp", 25, "smtp");
    insert_service(&mut map, "tcp", 53, "dns");
    insert_service(&mut map, "udp", 53, "dns");
    insert_service(&mut map, "tcp", 80, "http");
    insert_service(&mut map, "tcp", 110, "pop3");
    insert_service(&mut map, "tcp", 143, "imap");
    insert_service(&mut map, "tcp", 389, "ldap");
    insert_service(&mut map, "tcp", 443, "https");
    insert_service(&mut map, "tcp", 445, "smb");
    insert_service(&mut map, "tcp", 465, "smtps");
    insert_service(&mut map, "tcp", 587, "submission");
    insert_service(&mut map, "tcp", 993, "imaps");
    insert_service(&mut map, "tcp", 995, "pop3s");
    insert_service(&mut map, "tcp", 1433, "mssql");
    insert_service(&mut map, "tcp", 1521, "oracle");
    insert_service(&mut map, "tcp", 2049, "nfs");
    insert_service(&mut map, "udp", 2049, "nfs");
    insert_service(&mut map, "tcp", 2375, "docker");
    insert_service(&mut map, "tcp", 3306, "mysql");
    insert_service(&mut map, "tcp", 3389, "rdp");
    insert_service(&mut map, "tcp", 5432, "postgresql");
    insert_service(&mut map, "tcp", 5900, "vnc");
    insert_service(&mut map, "tcp", 6379, "redis");
    insert_service(&mut map, "tcp", 8080, "http-alt");
    insert_service(&mut map, "tcp", 8443, "https-alt");
    map
});

static INSECURE_SERVICES: Lazy<HashSet<String>> = Lazy::new(|| {
    HashSet::from([
        "telnet".to_string(),
        "ftp".to_string(),
        "pop3".to_string(),
        "imap".to_string(),
        "smtp".to_string(),
        "mysql".to_string(),
        "redis".to_string(),
        "rdp".to_string(),
        "vnc".to_string(),
    ])
});

fn insert_service(
    map: &mut BTreeMap<(String, u16), String>,
    protocol: &str,
    port: u16,
    name: &str,
) {
    map.insert((protocol.to_string(), port), name.to_string());
}

#[derive(Debug)]
struct ListenerProcessGroupBuilder {
    container: Option<String>,
    pid: i32,
    command: String,
    uid: u32,
    socket_count: usize,
    protocols: HashSet<String>,
    local_addresses: HashSet<String>,
}

impl ListenerProcessGroupBuilder {
    fn new(process: &SocketProcessInfo) -> Self {
        ListenerProcessGroupBuilder {
            container: process.container.clone(),
            pid: process.pid,
            command: process.command.clone(),
            uid: process.uid,
            socket_count: 0,
            protocols: HashSet::new(),
            local_addresses: HashSet::new(),
        }
    }

    fn finish(self) -> (Option<String>, ListenerProcessGroup) {
        let mut protocols: Vec<String> = self.protocols.into_iter().collect();
        protocols.sort();
        let mut local_addresses: Vec<String> = self.local_addresses.into_iter().collect();
        local_addresses.sort();

        (
            self.container,
            ListenerProcessGroup {
                pid: self.pid,
                command: self.command,
                uid: self.uid,
                socket_count: self.socket_count,
                protocols,
                local_addresses,
            },
        )
    }
}

#[derive(Debug)]
struct ListenerContainerGroupBuilder {
    container: Option<String>,
    socket_count: usize,
    process_count: usize,
    processes: Vec<ListenerProcessGroup>,
}

impl ListenerContainerGroupBuilder {
    fn new(container: Option<String>) -> Self {
        ListenerContainerGroupBuilder {
            container,
            socket_count: 0,
            process_count: 0,
            processes: Vec::new(),
        }
    }

    fn finish(mut self) -> ListenerContainerGroup {
        self.processes
            .sort_by(|a, b| b.socket_count.cmp(&a.socket_count));
        ListenerContainerGroup {
            container: self.container,
            socket_count: self.socket_count,
            process_count: self.process_count,
            processes: self.processes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_counts_total() {
        let counts = ListenerCounts {
            tcp: 2,
            tcp6: 1,
            udp: 3,
            udp6: 0,
        };
        assert_eq!(counts.total(), 6);
    }

    #[test]
    fn build_listener_groups_aggregates_by_container() {
        let samples = vec![
            SocketSample {
                protocol: "tcp".into(),
                local_address: "127.0.0.1:80".into(),
                state: Some("Listen".into()),
                processes: vec![SocketProcessInfo {
                    pid: 100,
                    command: "nginx".into(),
                    uid: 0,
                    container: Some("container_a".into()),
                }],
                service: Some("http".into()),
            },
            SocketSample {
                protocol: "tcp".into(),
                local_address: "127.0.0.1:443".into(),
                state: Some("Listen".into()),
                processes: vec![SocketProcessInfo {
                    pid: 100,
                    command: "nginx".into(),
                    uid: 0,
                    container: Some("container_a".into()),
                }],
                service: Some("https".into()),
            },
            SocketSample {
                protocol: "tcp".into(),
                local_address: "0.0.0.0:22".into(),
                state: Some("Listen".into()),
                processes: vec![SocketProcessInfo {
                    pid: 1,
                    command: "sshd".into(),
                    uid: 0,
                    container: None,
                }],
                service: Some("ssh".into()),
            },
        ];

        let groups = build_listener_groups(&samples);
        assert_eq!(groups.len(), 2);

        let container_group = groups
            .iter()
            .find(|group| group.container.as_deref() == Some("container_a"))
            .expect("container group");
        assert_eq!(container_group.socket_count, 2);
        assert_eq!(container_group.process_count, 1);
        assert_eq!(container_group.processes[0].socket_count, 2);
        assert_eq!(container_group.processes[0].protocols, vec!["tcp"]);

        let host_group = groups
            .iter()
            .find(|group| group.container.is_none())
            .expect("host group");
        assert_eq!(host_group.socket_count, 1);
        assert_eq!(host_group.processes[0].local_addresses, vec!["0.0.0.0:22"]);
    }

    #[test]
    fn derive_listener_insights_flags_wildcard_and_legacy() {
        let samples = vec![
            SocketSample {
                protocol: "tcp".into(),
                local_address: "0.0.0.0:23".into(),
                state: Some("Listen".into()),
                processes: vec![SocketProcessInfo {
                    pid: 42,
                    command: "inetd".into(),
                    uid: 0,
                    container: None,
                }],
                service: Some("telnet".into()),
            },
            SocketSample {
                protocol: "tcp".into(),
                local_address: "127.0.0.1:8080".into(),
                state: Some("Listen".into()),
                processes: vec![SocketProcessInfo {
                    pid: 200,
                    command: "app".into(),
                    uid: 1000,
                    container: Some("svc".into()),
                }],
                service: Some("http-alt".into()),
            },
        ];

        let insights = derive_listener_insights(&samples);
        assert_eq!(insights.len(), 2);

        let wildcard = insights
            .iter()
            .find(|insight| insight.rule == "wildcard_listener")
            .expect("wildcard rule");
        assert_eq!(wildcard.sockets.len(), 1);
        assert_eq!(wildcard.sockets[0].pid, Some(42));

        let legacy = insights
            .iter()
            .find(|insight| insight.rule == "legacy_protocol")
            .expect("legacy rule");
        assert_eq!(legacy.sockets[0].service.as_deref(), Some("telnet"));
    }
}
