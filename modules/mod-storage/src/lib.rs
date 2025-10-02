use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rustix::fs::{StatVfs, statvfs};
use serde::Serialize;
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};
use walkdir::WalkDir;

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
                let (worst_path, worst_ratio) = snapshot
                    .operating
                    .iter()
                    .filter(|mount| !mount.read_only)
                    .map(|mount| (mount.mount_point.as_str(), mount.usage_ratio))
                    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(("", 0.0));

                let summary = if worst_path.is_empty() {
                    format!(
                        "{} operating mounts, {:.1}% average usage",
                        snapshot.operating.len(),
                        snapshot.average_usage() * 100.0
                    )
                } else {
                    format!(
                        "{} operating mounts, worst {:.1}% at {}",
                        snapshot.operating.len(),
                        worst_ratio * 100.0,
                        worst_path
                    )
                };

                let body = json!({
                    "operating_mounts": snapshot.operating,
                    "pseudo_mounts": snapshot.pseudo,
                    "totals": snapshot.aggregate,
                    "docker": snapshot.docker,
                    "hotspots": snapshot.hotspots,
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
#[serde(rename_all = "snake_case")]
enum MountCategory {
    Operating,
    Pseudo,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct MountUsage {
    mount_point: String,
    source: String,
    fs_type: String,
    read_only: bool,
    category: MountCategory,
    operational: bool,
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    usage_ratio: f64,
    inodes_total: Option<u64>,
    inodes_used: Option<u64>,
    inodes_available: Option<u64>,
    inodes_usage_ratio: Option<f64>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct AggregateUsage {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct DockerStorageBreakdown {
    data_root: PathBuf,
    total_bytes: u64,
    overlay_bytes: u64,
    container_logs_bytes: u64,
    volumes_bytes: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct StorageSnapshot {
    operating: Vec<MountUsage>,
    pseudo: Vec<MountUsage>,
    aggregate: AggregateUsage,
    docker: Option<DockerStorageBreakdown>,
    hotspots: HotspotSummary,
}

impl StorageSnapshot {
    fn average_usage(&self) -> f64 {
        if self.operating.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.operating.iter().map(|m| m.usage_ratio).sum();
        sum / (self.operating.len() as f64)
    }
}

#[derive(Debug, Serialize, Clone, PartialEq, Default)]
struct HotspotSummary {
    directories: Vec<DirectoryHotspot>,
    logs: Vec<LogHotspot>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct DirectoryHotspot {
    path: String,
    size_bytes: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct LogHotspot {
    path: String,
    size_bytes: u64,
}

fn build_snapshot() -> Result<(StorageSnapshot, Vec<String>)> {
    let mounts = parse_proc_mounts(fs::read_to_string("/proc/mounts")?)
        .context("failed to parse /proc/mounts")?;

    let mut operating = Vec::new();
    let mut pseudo = Vec::new();
    let mut notes = Vec::new();

    for mount in mounts.iter() {
        match stat_for_mount(&mount.mount_point) {
            Ok(stat) => {
                let usage = MountUsage {
                    mount_point: mount.mount_point.clone(),
                    source: mount.source.clone(),
                    fs_type: mount.fs_type.clone(),
                    read_only: mount.is_read_only(),
                    category: classify_mount(&mount.fs_type),
                    operational: is_operational_mount(&mount.mount_point),
                    total_bytes: stat.total_bytes,
                    used_bytes: stat.used_bytes,
                    available_bytes: stat.available_bytes,
                    usage_ratio: stat.usage_ratio,
                    inodes_total: stat.inodes_total,
                    inodes_used: stat.inodes_used,
                    inodes_available: stat.inodes_available,
                    inodes_usage_ratio: stat.inodes_usage_ratio,
                };

                if usage.category == MountCategory::Pseudo {
                    pseudo.push(usage);
                    continue;
                }

                if mount.fs_type == "overlay" {
                    pseudo.push(usage);
                    continue;
                }
                operating.push(usage);
            }
            Err(err) => notes.push(format!(
                "Failed to read usage for {}: {}",
                mount.mount_point, err
            )),
        }
    }

    if operating.is_empty() {
        anyhow::bail!("no filesystem usage information available")
    }

    let aggregate = aggregate_usage(&operating);

    let docker_usage = match docker_storage_breakdown() {
        Some(Ok(usage)) => Some(usage),
        Some(Err(error)) => {
            notes.push(format!("Failed to summarize Docker storage: {error}"));
            None
        }
        None => None,
    };

    let (hotspots, mut hotspot_notes) = collect_hotspots(&operating);
    notes.append(&mut hotspot_notes);

    Ok((
        StorageSnapshot {
            operating,
            pseudo,
            aggregate,
            docker: docker_usage,
            hotspots,
        },
        notes,
    ))
}

#[derive(Debug, Clone)]
struct MountEntry {
    source: String,
    mount_point: String,
    fs_type: String,
    options: Vec<String>,
}

impl MountEntry {
    fn is_read_only(&self) -> bool {
        self.options.iter().any(|opt| opt == "ro")
    }
}

fn parse_proc_mounts(contents: String) -> Result<Vec<MountEntry>> {
    let mut entries = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let device = parts.next().unwrap_or_default();
        let mount_point = parts.next().unwrap_or_default();
        let fs_type = parts.next().unwrap_or_default();
        let options = parts
            .next()
            .unwrap_or_default()
            .split(',')
            .map(|opt| opt.to_string())
            .collect();

        entries.push(MountEntry {
            source: decode_mount_field(device),
            mount_point: decode_mount_field(mount_point),
            fs_type: decode_mount_field(fs_type),
            options,
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
    inodes_total: Option<u64>,
    inodes_used: Option<u64>,
    inodes_available: Option<u64>,
    inodes_usage_ratio: Option<f64>,
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
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let usage_ratio = if total_bytes == 0 {
        0.0
    } else {
        used_bytes as f64 / total_bytes as f64
    };

    let inode_stats = if vfs.f_files > 0 {
        let total = vfs.f_files;
        let avail = vfs.f_favail;
        let used = total.saturating_sub(vfs.f_ffree);
        let ratio = if total == 0 {
            None
        } else {
            Some(used as f64 / total as f64)
        };
        Some((total, used, avail, ratio))
    } else {
        None
    };

    Ok(MountStat {
        total_bytes,
        used_bytes,
        available_bytes,
        usage_ratio,
        inodes_total: inode_stats.map(|(total, _, _, _)| total),
        inodes_used: inode_stats.map(|(_, used, _, _)| used),
        inodes_available: inode_stats.map(|(_, _, avail, _)| avail),
        inodes_usage_ratio: inode_stats.and_then(|(_, _, _, ratio)| ratio),
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

fn decode_mount_field(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                match chars.peek() {
                    Some(digit) if digit.is_ascii_digit() => {
                        octal.push(*digit);
                        chars.next();
                    }
                    _ => break,
                }
            }

            if octal.len() == 3 {
                if let Ok(value) = u32::from_str_radix(&octal, 8) {
                    if let Some(ch) = char::from_u32(value) {
                        result.push(ch);
                        continue;
                    }
                }
            }

            result.push('\\');
            result.push_str(&octal);
        } else {
            result.push(ch);
        }
    }

    result
}

fn classify_mount(fs_type: &str) -> MountCategory {
    if PSEUDO_FS_TYPES.contains(&fs_type) {
        MountCategory::Pseudo
    } else {
        MountCategory::Operating
    }
}

fn is_operational_mount(mount_point: &str) -> bool {
    if mount_point == "/" {
        return true;
    }

    OPERATIONAL_PATHS.iter().any(|target| {
        mount_point == *target || target.starts_with(mount_point) || mount_point.starts_with(target)
    })
}

fn docker_storage_breakdown() -> Option<Result<DockerStorageBreakdown>> {
    const DOCKER_ROOT: &str = "/var/lib/docker";
    let root = Path::new(DOCKER_ROOT);
    if !root.exists() {
        return None;
    }

    Some(
        calculate_docker_storage(root).map(|(overlay, logs, volumes, total)| {
            DockerStorageBreakdown {
                data_root: root.to_path_buf(),
                total_bytes: total,
                overlay_bytes: overlay,
                container_logs_bytes: logs,
                volumes_bytes: volumes,
            }
        }),
    )
}

fn calculate_docker_storage(root: &Path) -> Result<(u64, u64, u64, u64)> {
    let overlay_path = root.join("overlay2");
    let containers_path = root.join("containers");
    let volumes_path = root.join("volumes");

    let overlay_bytes = directory_size(&overlay_path, None)?;
    let logs_bytes = containers_path
        .exists()
        .then(|| collect_container_logs_size(&containers_path))
        .transpose()?
        .unwrap_or(0);
    let volumes_bytes = directory_size(&volumes_path, None)?;

    let total_bytes = directory_size(root, None)?;

    Ok((overlay_bytes, logs_bytes, volumes_bytes, total_bytes))
}

fn collect_container_logs_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path).context("read containers directory")? {
        let entry = entry?;
        let log_path = entry.path().join("container.log");
        if let Ok(metadata) = fs::metadata(&log_path) {
            total = total.saturating_add(metadata.len());
        }
        let json_log = entry.path().join("hostconfig.json");
        if let Ok(metadata) = fs::metadata(&json_log) {
            total = total.saturating_add(metadata.len());
        }
        let stdout_log = entry.path().join("log.json");
        if let Ok(metadata) = fs::metadata(&stdout_log) {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn directory_size(path: &Path, max_depth: Option<usize>) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut walker = WalkDir::new(path).follow_links(false).into_iter();

    while let Some(entry) = walker.next() {
        match entry {
            Ok(entry) => {
                if let Some(depth) = max_depth {
                    if entry.depth() > depth {
                        if entry.file_type().is_dir() {
                            walker.skip_current_dir();
                        }
                        continue;
                    }
                }

                if entry.file_type().is_file() {
                    let metadata = entry.metadata()?;
                    total = total.saturating_add(metadata.len());
                }
            }
            Err(err) => {
                return Err(err.into());
            }
        }
    }

    Ok(total)
}

fn collect_hotspots(operating: &[MountUsage]) -> (HotspotSummary, Vec<String>) {
    const DIRECTORY_SCAN_DEPTH: usize = 3;
    const DIRECTORY_SAMPLE_PER_MOUNT: usize = 20;
    const DIRECTORY_LIMIT: usize = 5;
    const LOG_SCAN_DEPTH: usize = 2;
    const LOG_LIMIT: usize = 5;

    let mut notes = Vec::new();
    let mut directory_candidates = Vec::new();

    for mount in operating
        .iter()
        .filter(|mount| mount.operational && !mount.read_only)
    {
        let path = Path::new(&mount.mount_point);
        match collect_directory_hotspots(path, DIRECTORY_SCAN_DEPTH, DIRECTORY_SAMPLE_PER_MOUNT) {
            Ok(mut hotspots) => directory_candidates.append(&mut hotspots),
            Err(error) => notes.push(format!(
                "Failed to inspect {}: {}",
                mount.mount_point, error
            )),
        }
    }

    directory_candidates.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    directory_candidates.truncate(DIRECTORY_LIMIT);

    let (log_hotspots, mut log_notes) = collect_log_hotspots(Path::new("/var/log"), LOG_SCAN_DEPTH);
    notes.append(&mut log_notes);

    let logs = log_hotspots.into_iter().take(LOG_LIMIT).collect();

    (
        HotspotSummary {
            directories: directory_candidates,
            logs,
        },
        notes,
    )
}

fn collect_directory_hotspots(
    root: &Path,
    max_depth: usize,
    limit: usize,
) -> Result<Vec<DirectoryHotspot>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut hotspots = Vec::new();
    let mut processed = 0usize;

    for entry in fs::read_dir(root)? {
        if processed >= limit {
            break;
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }

        let size = directory_size(&entry.path(), Some(max_depth))?;
        hotspots.push(DirectoryHotspot {
            path: entry.path().display().to_string(),
            size_bytes: size,
        });
        processed += 1;
    }

    hotspots.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Ok(hotspots)
}

fn collect_log_hotspots(root: &Path, max_depth: usize) -> (Vec<LogHotspot>, Vec<String>) {
    const LOG_SCAN_CAP: usize = 512;

    if !root.is_dir() {
        return (Vec::new(), Vec::new());
    }

    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut examined = 0usize;

    let walker = WalkDir::new(root).max_depth(max_depth).follow_links(false);
    for entry in walker {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() {
                    match entry.metadata() {
                        Ok(metadata) => {
                            files.push(LogHotspot {
                                path: entry.path().display().to_string(),
                                size_bytes: metadata.len(),
                            });
                            examined += 1;
                            if examined >= LOG_SCAN_CAP {
                                break;
                            }
                        }
                        Err(error) => notes.push(format!(
                            "Failed to inspect log {}: {}",
                            entry.path().display(),
                            error
                        )),
                    }
                }
            }
            Err(error) => {
                notes.push(format!("Failed to traverse log directory: {error}"));
                break;
            }
        }
    }

    files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    (files, notes)
}

const PSEUDO_FS_TYPES: [&str; 13] = [
    "squashfs",
    "overlay",
    "tmpfs",
    "devtmpfs",
    "cgroup2",
    "proc",
    "sysfs",
    "nsfs",
    "ramfs",
    "zram",
    "fuse.snapfuse",
    "securityfs",
    "pstore",
];

const OPERATIONAL_PATHS: [&str; 7] = [
    "/",
    "/var",
    "/home",
    "/var/lib/docker",
    "/var/lib/containers",
    "/boot",
    "/boot/efi",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn decode_mount_field_unescapes_space() {
        let decoded = decode_mount_field("/snap/core20/\\0401234");
        assert_eq!(decoded, "/snap/core20/ 1234");
    }

    #[test]
    fn classify_mount_types() {
        assert!(matches!(classify_mount("ext4"), MountCategory::Operating));
        assert!(matches!(classify_mount("tmpfs"), MountCategory::Pseudo));
    }

    #[test]
    fn aggregate_usage_sums_values() {
        let mounts = vec![
            MountUsage {
                mount_point: "/".into(),
                source: "/dev/sda1".into(),
                fs_type: "ext4".into(),
                read_only: false,
                category: MountCategory::Operating,
                operational: true,
                total_bytes: 100,
                used_bytes: 40,
                available_bytes: 60,
                usage_ratio: 0.4,
                inodes_total: Some(1000),
                inodes_used: Some(400),
                inodes_available: Some(600),
                inodes_usage_ratio: Some(0.4),
            },
            MountUsage {
                mount_point: "/var".into(),
                source: "/dev/sda2".into(),
                fs_type: "ext4".into(),
                read_only: false,
                category: MountCategory::Operating,
                operational: true,
                total_bytes: 50,
                used_bytes: 10,
                available_bytes: 40,
                usage_ratio: 0.2,
                inodes_total: Some(1000),
                inodes_used: Some(200),
                inodes_available: Some(800),
                inodes_usage_ratio: Some(0.2),
            },
        ];

        let aggregate = aggregate_usage(&mounts);
        assert_eq!(aggregate.total_bytes, 150);
        assert_eq!(aggregate.used_bytes, 50);
        assert_eq!(aggregate.available_bytes, 100);
    }

    #[test]
    fn collect_directory_hotspots_prioritizes_larger() {
        let temp = tempdir().expect("tempdir");
        let large_dir = temp.path().join("large");
        let small_dir = temp.path().join("small");
        fs::create_dir_all(&large_dir).expect("create large");
        fs::create_dir_all(&small_dir).expect("create small");
        fs::write(large_dir.join("big.log"), vec![0u8; 2048]).expect("write big");
        fs::write(small_dir.join("tiny.log"), vec![0u8; 16]).expect("write tiny");

        let hotspots = collect_directory_hotspots(temp.path(), 1, 10).expect("hotspots");
        assert!(hotspots.len() >= 2);
        assert!(hotspots[0].path.ends_with("large"));
        assert!(hotspots[0].size_bytes >= hotspots[1].size_bytes);
    }

    #[test]
    fn collect_log_hotspots_limits_results() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("nested")).expect("create nested");
        fs::write(temp.path().join("app.log"), vec![0u8; 1024]).expect("write app");
        fs::write(
            temp.path().join("nested").join("service.log"),
            vec![0u8; 512],
        )
        .expect("write service");

        let (hotspots, notes) = collect_log_hotspots(temp.path(), 2);
        assert!(notes.is_empty());
        assert_eq!(hotspots.first().unwrap().size_bytes, 1024);
        assert!(hotspots[0].path.ends_with("app.log"));
    }
}
