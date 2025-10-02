use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use procfs::{Current, LoadAverage, Meminfo, process::Process};
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct ProcCollector;

impl Collector for ProcCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "proc",
            title: "Processes and Resources",
            description: "Overview of /proc: load and memory",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let snapshot = build_snapshot().context("failed to read /proc metrics")?;
        Ok(section_from_snapshot(&snapshot))
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(ProcCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, PartialEq)]
struct ProcSnapshot {
    loadavg: Option<(f32, f32, f32)>,
    memory: MemorySnapshot,
    psi: Option<PsiSnapshot>,
    notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct MemorySnapshot {
    host: HostMemory,
    cgroup: Option<CgroupMemorySnapshot>,
    swap: SwapSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
struct HostMemory {
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
    used_bytes: Option<u64>,
    usage_ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct CgroupMemorySnapshot {
    path: String,
    limit_bytes: Option<u64>,
    usage_bytes: Option<u64>,
    usage_ratio: Option<f64>,
    swap_limit_bytes: Option<u64>,
    swap_usage_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
struct SwapSnapshot {
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
    devices: Vec<SwapDevice>,
    zram_devices: Vec<ZramDevice>,
}

#[derive(Debug, Clone, PartialEq)]
struct SwapDevice {
    name: String,
    kind: String,
    size_bytes: u64,
    used_bytes: u64,
    priority: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct ZramDevice {
    name: String,
    disksize_bytes: u64,
    compressed_bytes: Option<u64>,
    mem_used_bytes: Option<u64>,
    active: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct PsiSnapshot {
    cpu: Option<PsiResource>,
    memory: Option<PsiResource>,
    io: Option<PsiResource>,
}

#[derive(Debug, Clone, PartialEq)]
struct PsiResource {
    some: Option<PsiMetrics>,
    full: Option<PsiMetrics>,
}

#[derive(Debug, Clone, PartialEq)]
struct PsiMetrics {
    avg10: f64,
    avg60: f64,
    avg300: f64,
    total: u64,
}

fn build_snapshot() -> Result<ProcSnapshot> {
    let loadavg = LoadAverage::current()
        .ok()
        .map(|l| (l.one, l.five, l.fifteen));

    let (memory, notes) = collect_memory_snapshot()?;
    let psi = collect_psi_snapshot();

    Ok(ProcSnapshot {
        loadavg,
        memory,
        psi,
        notes,
    })
}

fn collect_memory_snapshot() -> Result<(MemorySnapshot, Vec<String>)> {
    let mut notes = Vec::new();
    let meminfo = Meminfo::current().ok();

    let host = meminfo
        .as_ref()
        .map(host_memory_from_meminfo)
        .unwrap_or_else(|| HostMemory {
            total_bytes: None,
            available_bytes: None,
            used_bytes: None,
            usage_ratio: None,
        });

    let swap_total_bytes = meminfo
        .as_ref()
        .map(|info| info.swap_total)
        .map(|kb| kb.saturating_mul(1024));
    let swap_free_bytes = meminfo
        .as_ref()
        .map(|info| info.swap_free)
        .map(|kb| kb.saturating_mul(1024));

    let devices = match collect_swap_devices() {
        Ok(devices) => devices,
        Err(err) => {
            notes.push(format!("Failed to read /proc/swaps: {err}"));
            Vec::new()
        }
    };

    let active_swaps: HashSet<String> = devices.iter().map(|device| device.name.clone()).collect();

    let zram_devices = match collect_zram_devices(&active_swaps) {
        Ok(devices) => devices,
        Err(err) => {
            notes.push(format!("Failed to inspect zram devices: {err}"));
            Vec::new()
        }
    };

    if matches!(swap_total_bytes, Some(0)) && !zram_devices.is_empty() {
        notes.push(
            "SwapTotal is 0 while zram devices are present; zram swap may not be activated"
                .to_string(),
        );
    }

    let cgroup = match collect_cgroup_memory() {
        Ok(value) => value,
        Err(err) => {
            notes.push(format!("Failed to collect cgroup memory stats: {err}"));
            None
        }
    };

    let swap = SwapSnapshot {
        total_bytes: swap_total_bytes,
        free_bytes: swap_free_bytes,
        devices,
        zram_devices,
    };

    Ok((MemorySnapshot { host, cgroup, swap }, notes))
}

fn collect_psi_snapshot() -> Option<PsiSnapshot> {
    let cpu = read_psi_resource("/proc/pressure/cpu");
    let memory = read_psi_resource("/proc/pressure/memory");
    let io = read_psi_resource("/proc/pressure/io");

    if cpu.is_none() && memory.is_none() && io.is_none() {
        None
    } else {
        Some(PsiSnapshot { cpu, memory, io })
    }
}

fn host_memory_from_meminfo(meminfo: &Meminfo) -> HostMemory {
    let total_bytes = Some(meminfo.mem_total.saturating_mul(1024));
    let available_kb = meminfo.mem_available.or(Some(meminfo.mem_free));
    let available_bytes = available_kb.map(|kb| kb.saturating_mul(1024));

    let used_bytes = match (total_bytes, available_bytes) {
        (Some(total), Some(available)) => Some(total.saturating_sub(available)),
        _ => None,
    };

    let usage_ratio = match (used_bytes, total_bytes) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64),
        _ => None,
    };

    HostMemory {
        total_bytes,
        available_bytes,
        used_bytes,
        usage_ratio,
    }
}

fn collect_swap_devices() -> Result<Vec<SwapDevice>> {
    let content = fs::read_to_string("/proc/swaps").context("failed to read /proc/swaps")?;
    let mut devices = Vec::new();

    for line in content.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let size_bytes = parts[2].parse::<u64>().unwrap_or(0).saturating_mul(1024);
        let used_bytes = parts[3].parse::<u64>().unwrap_or(0).saturating_mul(1024);
        let priority = parts[4].parse::<i64>().unwrap_or(0);

        devices.push(SwapDevice {
            name: parts[0].to_string(),
            kind: parts[1].to_string(),
            size_bytes,
            used_bytes,
            priority,
        });
    }

    Ok(devices)
}

fn collect_zram_devices(active_swaps: &HashSet<String>) -> Result<Vec<ZramDevice>> {
    let sys_block = match fs::read_dir("/sys/block") {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };

    let mut devices = Vec::new();
    for entry in sys_block {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                return Err(err.into());
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("zram") {
            continue;
        }

        let path = entry.path();
        let disksize_bytes = read_u64_silent(path.join("disksize")).unwrap_or(0);
        let compressed_bytes = read_u64_silent(path.join("compr_data_size"));
        let mem_used_bytes = read_u64_silent(path.join("mem_used_total"));
        let device_path = format!("/dev/{}", name);
        let active = active_swaps.contains(&device_path);

        devices.push(ZramDevice {
            name: device_path,
            disksize_bytes,
            compressed_bytes,
            mem_used_bytes,
            active,
        });
    }

    Ok(devices)
}

fn collect_cgroup_memory() -> Result<Option<CgroupMemorySnapshot>> {
    let process = match Process::myself() {
        Ok(process) => process,
        Err(_) => return Ok(None),
    };
    let groups = match process.cgroups() {
        Ok(groups) => groups,
        Err(_) => return Ok(None),
    };

    #[derive(Clone, Copy)]
    enum CgroupVersion {
        Unified,
        Legacy,
    }

    let mut candidates: Vec<(CgroupVersion, String)> = Vec::new();
    for group in &groups.0 {
        if group.controllers.is_empty() {
            candidates.push((CgroupVersion::Unified, group.pathname.clone()));
        }
        if group
            .controllers
            .iter()
            .any(|controller| controller == "memory")
        {
            candidates.push((CgroupVersion::Legacy, group.pathname.clone()));
        }
    }

    for (version, relative) in candidates {
        match version {
            CgroupVersion::Unified => {
                let dir = join_cgroup_path(Path::new("/sys/fs/cgroup"), &relative);
                if let Some(snapshot) = read_cgroup_v2_memory(&dir, &relative)? {
                    return Ok(Some(snapshot));
                }
            }
            CgroupVersion::Legacy => {
                let dir = join_cgroup_path(Path::new("/sys/fs/cgroup/memory"), &relative);
                if let Some(snapshot) = read_cgroup_v1_memory(&dir, &relative)? {
                    return Ok(Some(snapshot));
                }
            }
        }
    }

    Ok(None)
}

fn join_cgroup_path(base: &Path, relative: &str) -> PathBuf {
    if relative == "/" {
        base.to_path_buf()
    } else {
        base.join(relative.trim_start_matches('/'))
    }
}

fn read_cgroup_v2_memory(dir: &Path, relative: &str) -> Result<Option<CgroupMemorySnapshot>> {
    if !dir.exists() {
        return Ok(None);
    }

    let usage_bytes = read_u64_from_file(dir.join("memory.current"))?;
    let limit_bytes = read_u64_from_file(dir.join("memory.max"))?;
    let swap_usage_bytes = read_u64_from_file(dir.join("memory.swap.current"))?;
    let swap_limit_bytes = read_u64_from_file(dir.join("memory.swap.max"))?;

    let usage_ratio = match (usage_bytes, limit_bytes) {
        (Some(usage), Some(limit)) if limit > 0 => Some(usage as f64 / limit as f64),
        _ => None,
    };

    Ok(Some(CgroupMemorySnapshot {
        path: if relative.is_empty() {
            "/".to_string()
        } else {
            relative.to_string()
        },
        limit_bytes,
        usage_bytes,
        usage_ratio,
        swap_limit_bytes,
        swap_usage_bytes,
    }))
}

fn read_cgroup_v1_memory(dir: &Path, relative: &str) -> Result<Option<CgroupMemorySnapshot>> {
    if !dir.exists() {
        return Ok(None);
    }

    let usage_bytes = read_u64_from_file(dir.join("memory.usage_in_bytes"))?;
    let limit_bytes = read_u64_from_file(dir.join("memory.limit_in_bytes"))?;
    let swap_usage_bytes = read_u64_from_file(dir.join("memory.memsw.usage_in_bytes"))?;
    let swap_limit_bytes = read_u64_from_file(dir.join("memory.memsw.limit_in_bytes"))?;

    let usage_ratio = match (usage_bytes, limit_bytes) {
        (Some(usage), Some(limit)) if limit > 0 => Some(usage as f64 / limit as f64),
        _ => None,
    };

    Ok(Some(CgroupMemorySnapshot {
        path: if relative.is_empty() {
            "/".to_string()
        } else {
            relative.to_string()
        },
        limit_bytes,
        usage_bytes,
        usage_ratio,
        swap_limit_bytes,
        swap_usage_bytes,
    }))
}

fn read_psi_resource(path: &str) -> Option<PsiResource> {
    let content = fs::read_to_string(path).ok()?;
    let mut resource = PsiResource {
        some: None,
        full: None,
    };

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let label = parts.next()?;
        let mut metrics = PsiMetrics {
            avg10: 0.0,
            avg60: 0.0,
            avg300: 0.0,
            total: 0,
        };

        for part in parts {
            let mut kv = part.split('=');
            let key = kv.next();
            let value = kv.next();
            if let (Some(key), Some(value)) = (key, value) {
                match key {
                    "avg10" => metrics.avg10 = value.parse().unwrap_or(0.0),
                    "avg60" => metrics.avg60 = value.parse().unwrap_or(0.0),
                    "avg300" => metrics.avg300 = value.parse().unwrap_or(0.0),
                    "total" => metrics.total = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        match label {
            "some" => resource.some = Some(metrics),
            "full" => resource.full = Some(metrics),
            _ => {}
        }
    }

    if resource.some.is_none() && resource.full.is_none() {
        None
    } else {
        Some(resource)
    }
}

fn read_u64_from_file(path: impl AsRef<Path>) -> Result<Option<u64>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("max") {
        return Ok(None);
    }
    let value = trimmed.parse::<u64>()?;
    if value >= u64::MAX - 1 {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn read_u64_silent(path: impl AsRef<Path>) -> Option<u64> {
    read_u64_from_file(path).ok().flatten()
}

fn psi_resource_to_value(resource: &PsiResource) -> serde_json::Value {
    json!({
        "some": resource.some.as_ref().map(psi_metrics_to_value),
        "full": resource.full.as_ref().map(psi_metrics_to_value),
    })
}

fn psi_metrics_to_value(metrics: &PsiMetrics) -> serde_json::Value {
    json!({
        "avg10": metrics.avg10,
        "avg60": metrics.avg60,
        "avg300": metrics.avg300,
        "total": metrics.total,
    })
}

fn bytes_to_gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn section_from_snapshot(snapshot: &ProcSnapshot) -> Section {
    let body = json!({
        "loadavg": snapshot.loadavg.map(|(one, five, fifteen)| {
            json!({
                "one": one,
                "five": five,
                "fifteen": fifteen,
            })
        }),
        "memory": {
            "host": {
                "total_bytes": snapshot.memory.host.total_bytes,
                "available_bytes": snapshot.memory.host.available_bytes,
                "used_bytes": snapshot.memory.host.used_bytes,
                "usage_ratio": snapshot.memory.host.usage_ratio,
            },
            "cgroup": snapshot.memory.cgroup.as_ref().map(|cg| json!({
                "path": cg.path,
                "limit_bytes": cg.limit_bytes,
                "usage_bytes": cg.usage_bytes,
                "usage_ratio": cg.usage_ratio,
                "swap_limit_bytes": cg.swap_limit_bytes,
                "swap_usage_bytes": cg.swap_usage_bytes,
            })),
            "swap": {
                "total_bytes": snapshot.memory.swap.total_bytes,
                "free_bytes": snapshot.memory.swap.free_bytes,
                "devices": snapshot
                    .memory
                    .swap
                    .devices
                    .iter()
                    .map(|device| {
                        json!({
                            "name": device.name,
                            "kind": device.kind,
                            "size_bytes": device.size_bytes,
                            "used_bytes": device.used_bytes,
                            "priority": device.priority,
                        })
                    })
                    .collect::<Vec<_>>(),
                "zram_devices": snapshot
                    .memory
                    .swap
                    .zram_devices
                    .iter()
                    .map(|device| {
                        json!({
                            "name": device.name,
                            "disksize_bytes": device.disksize_bytes,
                            "compressed_bytes": device.compressed_bytes,
                            "mem_used_bytes": device.mem_used_bytes,
                            "active": device.active,
                        })
                    })
                    .collect::<Vec<_>>(),
            }
        },
        "psi": snapshot.psi.as_ref().map(|psi| json!({
            "cpu": psi.cpu.as_ref().map(|res| psi_resource_to_value(res)),
            "memory": psi.memory.as_ref().map(|res| psi_resource_to_value(res)),
            "io": psi.io.as_ref().map(|res| psi_resource_to_value(res)),
        })),
    });

    let mut section = Section::success("proc", "Processes and Resources", body);
    section.summary = Some(snapshot.summary());
    section.notes = snapshot.notes.clone();
    section
}

impl ProcSnapshot {
    fn summary(&self) -> String {
        let load = self
            .loadavg
            .map(|(one, _, _)| format!("LoadAvg 1m: {:.2}", one))
            .unwrap_or_else(|| "LoadAvg unavailable".to_string());

        if let (Some(used), Some(total)) =
            (self.memory.host.used_bytes, self.memory.host.total_bytes)
        {
            if total > 0 {
                let ratio = used as f64 / total as f64;
                let available = self
                    .memory
                    .host
                    .available_bytes
                    .unwrap_or(total.saturating_sub(used));
                let available_gib = bytes_to_gib(available);
                return format!(
                    "{}, Mem used {:.1}% ({:.1} GiB free)",
                    load,
                    ratio * 100.0,
                    available_gib
                );
            }
        }

        load
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_with_loadavg() {
        let snapshot = ProcSnapshot {
            loadavg: Some((0.5, 0.4, 0.3)),
            memory: MemorySnapshot {
                host: HostMemory {
                    total_bytes: Some(1_073_741_824),
                    available_bytes: Some(536_870_912),
                    used_bytes: Some(536_870_912),
                    usage_ratio: Some(0.5),
                },
                cgroup: None,
                swap: SwapSnapshot {
                    total_bytes: Some(536_870_912),
                    free_bytes: Some(268_435_456),
                    devices: Vec::new(),
                    zram_devices: Vec::new(),
                },
            },
            psi: None,
            notes: Vec::new(),
        };

        assert_eq!(
            snapshot.summary(),
            "LoadAvg 1m: 0.50, Mem used 50.0% (0.5 GiB free)"
        );
    }

    #[test]
    fn section_contains_memory_totals() {
        let snapshot = ProcSnapshot {
            loadavg: None,
            memory: MemorySnapshot {
                host: HostMemory {
                    total_bytes: Some(2_147_483_648),
                    available_bytes: Some(1_073_741_824),
                    used_bytes: Some(1_073_741_824),
                    usage_ratio: Some(0.5),
                },
                cgroup: None,
                swap: SwapSnapshot {
                    total_bytes: None,
                    free_bytes: None,
                    devices: Vec::new(),
                    zram_devices: Vec::new(),
                },
            },
            psi: None,
            notes: Vec::new(),
        };

        let section = section_from_snapshot(&snapshot);
        let mem = section
            .body
            .get("memory")
            .and_then(|value| value.get("host"))
            .unwrap();
        assert_eq!(
            mem.get("total_bytes").and_then(|v| v.as_u64()),
            Some(2_147_483_648)
        );
    }
}
