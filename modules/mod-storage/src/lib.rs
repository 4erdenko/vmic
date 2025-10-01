use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use rustix::fs::{StatVfs, statvfs};
use serde::Serialize;
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct StorageCollector;

impl Collector for StorageCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "storage",
            title: "Storage Overview",
            description: "Filesystem usage across mounted volumes",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match build_snapshot() {
            Ok((snapshot, notes)) => {
                let summary = format!(
                    "{} mounts, {:.1}% average usage",
                    snapshot.mounts.len(),
                    snapshot.average_usage() * 100.0
                );

                let body = json!({
                    "mounts": snapshot.mounts,
                    "totals": snapshot.aggregate,
                });

                let mut section = Section::success("storage", "Storage Overview", body);
                section.summary = Some(summary);
                section.notes = notes;
                Ok(section)
            }
            Err(error) => Ok(Section::degraded(
                "storage",
                "Storage Overview",
                error.to_string(),
                json!({ "mounts": [], "totals": {} }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(StorageCollector)
}

register_collector!(create_collector);

#[derive(Debug, Serialize, Clone, PartialEq)]
struct MountUsage {
    mount_point: String,
    fs_type: String,
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    usage_ratio: f64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct AggregateUsage {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct StorageSnapshot {
    mounts: Vec<MountUsage>,
    aggregate: AggregateUsage,
}

impl StorageSnapshot {
    fn average_usage(&self) -> f64 {
        if self.mounts.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.mounts.iter().map(|m| m.usage_ratio).sum();
        sum / (self.mounts.len() as f64)
    }
}

fn build_snapshot() -> Result<(StorageSnapshot, Vec<String>)> {
    let mounts = parse_proc_mounts(fs::read_to_string("/proc/mounts")?)
        .context("failed to parse /proc/mounts")?;

    let mut usages = Vec::new();
    let mut notes = Vec::new();

    for mount in mounts.iter() {
        match stat_for_mount(&mount.mount_point) {
            Ok(usage) => usages.push(MountUsage {
                mount_point: mount.mount_point.clone(),
                fs_type: mount.fs_type.clone(),
                total_bytes: usage.total_bytes,
                used_bytes: usage.used_bytes,
                available_bytes: usage.available_bytes,
                usage_ratio: usage.usage_ratio,
            }),
            Err(err) => notes.push(format!(
                "Failed to read usage for {}: {}",
                mount.mount_point, err
            )),
        }
    }

    if usages.is_empty() {
        anyhow::bail!("no filesystem usage information available")
    }

    let aggregate = aggregate_usage(&usages);

    Ok((
        StorageSnapshot {
            mounts: usages,
            aggregate,
        },
        notes,
    ))
}

#[derive(Debug, Clone)]
struct MountEntry {
    mount_point: String,
    fs_type: String,
}

fn parse_proc_mounts(contents: String) -> Result<Vec<MountEntry>> {
    let mut entries = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _device = parts.next();
        let mount_point = parts.next().unwrap_or_default();
        let fs_type = parts.next().unwrap_or_default();

        if mount_point.starts_with("/proc")
            || mount_point.starts_with("/sys")
            || mount_point.starts_with("/run")
            || mount_point.starts_with("/dev")
        {
            continue;
        }

        entries.push(MountEntry {
            mount_point: mount_point.to_string(),
            fs_type: fs_type.to_string(),
        });
    }

    entries.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    entries.dedup_by(|a, b| a.mount_point == b.mount_point);
    Ok(entries)
}

#[derive(Debug, Clone)]
struct MountStat {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    usage_ratio: f64,
}

fn stat_for_mount<P: AsRef<Path>>(path: P) -> Result<MountStat> {
    let vfs: StatVfs = statvfs(path.as_ref()).context("statvfs failed")?;
    let block_size = if vfs.f_frsize > 0 {
        vfs.f_frsize
    } else {
        vfs.f_bsize
    };
    let total_bytes = vfs.f_blocks.saturating_mul(block_size);
    let available_bytes = vfs.f_bavail.saturating_mul(block_size);
    let free_bytes = vfs.f_bfree.saturating_mul(block_size);
    let used_bytes = total_bytes.saturating_sub(free_bytes);
    let usage_ratio = if total_bytes == 0 {
        0.0
    } else {
        used_bytes as f64 / total_bytes as f64
    };

    Ok(MountStat {
        total_bytes,
        used_bytes,
        available_bytes,
        usage_ratio,
    })
}

fn aggregate_usage(mounts: &[MountUsage]) -> AggregateUsage {
    let mut total = 0u64;
    let mut used = 0u64;
    let mut available = 0u64;

    for mount in mounts {
        total = total.saturating_add(mount.total_bytes);
        used = used.saturating_add(mount.used_bytes);
        available = available.saturating_add(mount.available_bytes);
    }

    AggregateUsage {
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mounts_skips_virtual_fs() {
        let sample = "proc /proc proc rw 0 0\ntmpfs /run tmpfs rw 0 0\n/dev/sda1 / ext4 rw 0 0\n";
        let result = parse_proc_mounts(sample.to_string()).expect("parse");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].mount_point, "/");
    }

    #[test]
    fn aggregate_usage_sums_values() {
        let mounts = vec![
            MountUsage {
                mount_point: "/".into(),
                fs_type: "ext4".into(),
                total_bytes: 100,
                used_bytes: 40,
                available_bytes: 60,
                usage_ratio: 0.4,
            },
            MountUsage {
                mount_point: "/var".into(),
                fs_type: "ext4".into(),
                total_bytes: 50,
                used_bytes: 10,
                available_bytes: 40,
                usage_ratio: 0.2,
            },
        ];

        let aggregate = aggregate_usage(&mounts);
        assert_eq!(aggregate.total_bytes, 150);
        assert_eq!(aggregate.used_bytes, 50);
        assert_eq!(aggregate.available_bytes, 100);
    }
}
