use anyhow::{Context as _, Result};
use procfs::net::{self, TcpState};
use serde::Serialize;
use serde_json::json;
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
                json!({ "interfaces": [], "listeners": {"counts": ListenerCounts::default(), "samples": []} }),
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
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ListenerSnapshot {
    counts: ListenerCounts,
    samples: Vec<SocketSample>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct NetworkSnapshot {
    interfaces: Vec<InterfaceInfo>,
    listeners: ListenerSnapshot,
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

    match net::tcp() {
        Ok(entries) => {
            for entry in entries.into_iter().filter(|e| e.state == TcpState::Listen) {
                counts.tcp += 1;
                if samples.len() < MAX_SOCKET_SAMPLES {
                    samples.push(SocketSample {
                        protocol: "tcp".into(),
                        local_address: format!("{}", entry.local_address),
                        state: Some(format!("{:?}", entry.state)),
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
                    samples.push(SocketSample {
                        protocol: "tcp6".into(),
                        local_address: format!("{}", entry.local_address),
                        state: Some(format!("{:?}", entry.state)),
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
                samples.push(SocketSample {
                    protocol: "udp".into(),
                    local_address: format!("{}", entry.local_address),
                    state: None,
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
                samples.push(SocketSample {
                    protocol: "udp6".into(),
                    local_address: format!("{}", entry.local_address),
                    state: None,
                });
            }
        }
        Err(err) => notes.push(format!("Failed to read /proc/net/udp6: {}", err)),
    }

    (ListenerSnapshot { counts, samples }, notes)
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
}
