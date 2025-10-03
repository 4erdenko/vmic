use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
#[cfg(feature = "client")]
use std::collections::HashMap;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct DockerCollector;

impl Collector for DockerCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "docker",
            title: "Docker Containers",
            description: "Docker Engine and container status",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        #[cfg(feature = "client")]
        {
            match collect_docker_snapshot() {
                Ok(snapshot) => {
                    let body = json!({
                        "engine": snapshot.engine,
                        "containers": snapshot.containers,
                        "notes": snapshot.notes,
                        "storage": snapshot.storage,
                    });
                    let mut section = Section::success("docker", "Docker Containers", body);
                    section.summary = Some(format!(
                        "{} containers discovered",
                        snapshot.containers.len()
                    ));
                    if !snapshot.notes.is_empty() {
                        section.notes = snapshot.notes.clone();
                    }
                    Ok(section)
                }
                Err(err) => Ok(Section::degraded(
                    "docker",
                    "Docker Containers",
                    err.to_string(),
                    json!({
                        "engine": json!({ "status": "unavailable" }),
                        "containers": Vec::<serde_json::Value>::new(),
                        "storage": serde_json::Value::Null,
                    }),
                )),
            }
        }

        #[cfg(not(feature = "client"))]
        {
            Ok(Section::degraded(
                "docker",
                "Docker Containers",
                "Docker feature is disabled or dependencies are unavailable".to_string(),
                json!({
                    "engine": json!({ "status": "unavailable" }),
                    "containers": Vec::<serde_json::Value>::new(),
                    "notes": Vec::<String>::new(),
                    "storage": serde_json::Value::Null,
                }),
            ))
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(DockerCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct EngineInfo {
    version: Option<String>,
    api_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ContainerInfo {
    id: String,
    names: Vec<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
    metrics: Option<ContainerMetrics>,
    health: Option<String>,
    health_failing_streak: Option<u64>,
    restart_count: Option<u64>,
    size_rw_bytes: Option<u64>,
    size_root_fs_bytes: Option<u64>,
    mounts: Vec<ContainerMountInfo>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct DockerSnapshot {
    engine: Option<EngineInfo>,
    containers: Vec<ContainerInfo>,
    notes: Vec<String>,
    storage: Option<DockerStorageSummary>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct DockerStorageSummary {
    image_total_bytes: Option<u64>,
    image_count: usize,
    volume_total_bytes: Option<u64>,
    volume_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct ContainerMetrics {
    cpu_percent: Option<f64>,
    memory_usage_bytes: Option<u64>,
    memory_limit_bytes: Option<u64>,
    memory_percent: Option<f64>,
    network_rx_bytes: Option<u64>,
    network_tx_bytes: Option<u64>,
    block_read_bytes: Option<u64>,
    block_write_bytes: Option<u64>,
}

impl ContainerInfo {
    fn with_metrics(mut self, metrics: Option<ContainerMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    fn apply_details(&mut self, details: ContainerDetails) {
        if let Some(health) = details.health_status {
            self.health = Some(health);
        }
        if let Some(streak) = details.health_failing_streak {
            self.health_failing_streak = Some(streak);
        }
        if let Some(restart_count) = details.restart_count {
            self.restart_count = Some(restart_count);
        }
        if let Some(size_rw) = details.size_rw_bytes {
            self.size_rw_bytes = Some(size_rw);
        }
        if let Some(size_root) = details.size_root_fs_bytes {
            self.size_root_fs_bytes = Some(size_root);
        }
        if !details.mounts.is_empty() {
            self.mounts = details.mounts;
        }
    }
}

#[cfg(feature = "client")]
fn collect_docker_snapshot() -> Result<DockerSnapshot> {
    use bollard::Docker;
    use bollard::query_parameters::ListContainersOptionsBuilder;
    use std::default::Default;
    use tokio::runtime::Builder;

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")?;

    runtime.block_on(async {
        let docker =
            Docker::connect_with_local_defaults().context("failed to connect to Docker daemon")?;

        let version = docker
            .version()
            .await
            .context("failed to query Docker version")?;

        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .size(true)
            .build();

        let containers = docker
            .list_containers(Some(options))
            .await
            .context("failed to list containers")?;

        let engine = EngineInfo {
            version: version.version,
            api_version: version.api_version,
        };

        let stats_options = bollard::query_parameters::StatsOptionsBuilder::default()
            .stream(false)
            .one_shot(true)
            .build();

        let (storage, volume_sizes, mut storage_notes) =
            match collect_storage_summary(&docker).await {
                Ok(result) => result,
                Err(error) => (
                    None,
                    HashMap::new(),
                    vec![format!("Failed to summarize Docker storage: {error}")],
                ),
            };

        let (containers, mut notes) =
            collect_containers_with_details(&docker, containers, &stats_options, &volume_sizes)
                .await;

        notes.append(&mut storage_notes);

        Ok(DockerSnapshot {
            engine: Some(engine),
            containers,
            notes,
            storage,
        })
    })
}

#[cfg(feature = "client")]
const METRICS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct ContainerMountInfo {
    destination: String,
    source: Option<String>,
    r#type: Option<String>,
    driver: Option<String>,
    rw: Option<bool>,
    volume_name: Option<String>,
    size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct ContainerDetails {
    health_status: Option<String>,
    health_failing_streak: Option<u64>,
    restart_count: Option<u64>,
    size_rw_bytes: Option<u64>,
    size_root_fs_bytes: Option<u64>,
    mounts: Vec<ContainerMountInfo>,
}

#[cfg(feature = "client")]
async fn collect_containers_with_details(
    docker: &bollard::Docker,
    containers: Vec<bollard::models::ContainerSummary>,
    stats_options: &bollard::query_parameters::StatsOptions,
    volume_sizes: &HashMap<String, u64>,
) -> (Vec<ContainerInfo>, Vec<String>) {
    let mut enriched = Vec::with_capacity(containers.len());
    let mut notes = Vec::new();

    for summary in containers {
        let mut info = ContainerInfo::from(summary);
        let container_id = info.id.clone();

        match fetch_container_metrics(docker, &container_id, stats_options).await {
            Ok(metrics) => {
                info = info.with_metrics(Some(metrics));
            }
            Err(error) => {
                let name = info
                    .names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| container_id.clone());
                notes.push(format!(
                    "Failed to collect stats for container {}: {}",
                    name, error
                ));
            }
        }

        match fetch_container_details(docker, &container_id, volume_sizes).await {
            Ok(details) => {
                if let Some(health) = details.health_status.as_deref() {
                    if health.eq_ignore_ascii_case("unhealthy") {
                        let name = info
                            .names
                            .first()
                            .cloned()
                            .unwrap_or_else(|| container_id.clone());
                        notes.push(format!("Container {} reported unhealthy status", name));
                    }
                }
                info.apply_details(details);
            }
            Err(error) => {
                let name = info
                    .names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| container_id.clone());
                notes.push(format!("Failed to inspect container {}: {}", name, error));
            }
        }

        enriched.push(info);
    }

    (enriched, notes)
}

#[cfg(feature = "client")]
async fn fetch_container_metrics(
    docker: &bollard::Docker,
    container_id: &str,
    options: &bollard::query_parameters::StatsOptions,
) -> Result<ContainerMetrics> {
    use anyhow::anyhow;
    use futures_util::TryStreamExt;
    use tokio::time::timeout;

    let mut stream = docker.stats(container_id, Some(options.clone()));
    let next = timeout(METRICS_TIMEOUT, stream.try_next())
        .await
        .context("timed out waiting for container stats")?;

    match next {
        Ok(Some(stats)) => Ok(ContainerMetrics::from_stats(&stats)),
        Ok(None) => Err(anyhow!("docker returned no stats for container")),
        Err(error) => Err(error.into()),
    }
}

#[cfg(feature = "client")]
async fn fetch_container_details(
    docker: &bollard::Docker,
    container_id: &str,
    volume_sizes: &HashMap<String, u64>,
) -> Result<ContainerDetails> {
    use bollard::query_parameters::InspectContainerOptionsBuilder;

    let inspect_options = InspectContainerOptionsBuilder::default().size(true).build();

    let response = docker
        .inspect_container(container_id, Some(inspect_options))
        .await?;

    let mut details = ContainerDetails::default();

    if let Some(state) = response.state {
        if let Some(health) = state.health {
            if let Some(status) = health.status {
                details.health_status = Some(status.to_string());
            }
            if let Some(streak) = health.failing_streak {
                if streak >= 0 {
                    details.health_failing_streak = Some(streak as u64);
                }
            }
        }
    }

    if let Some(restart_count) = response.restart_count {
        if restart_count >= 0 {
            details.restart_count = Some(restart_count as u64);
        }
    }

    details.size_rw_bytes = normalize_size(response.size_rw);
    details.size_root_fs_bytes = normalize_size(response.size_root_fs);

    if let Some(mounts) = response.mounts {
        details.mounts = mounts
            .into_iter()
            .map(|mount| ContainerMountInfo::from_mount(mount, volume_sizes))
            .collect();
    }

    Ok(details)
}

#[cfg(feature = "client")]
fn normalize_size(value: Option<i64>) -> Option<u64> {
    match value {
        Some(size) if size >= 0 => Some(size as u64),
        _ => None,
    }
}

#[cfg(feature = "client")]
impl ContainerMountInfo {
    fn from_mount(
        mount: bollard::models::MountPoint,
        volume_sizes: &HashMap<String, u64>,
    ) -> ContainerMountInfo {
        let destination = mount.destination.unwrap_or_default();
        let volume_name = mount.name.clone();
        let size_bytes = volume_name
            .as_ref()
            .and_then(|name| volume_sizes.get(name))
            .copied();

        ContainerMountInfo {
            destination,
            source: mount.source,
            r#type: mount.typ.map(|t| t.to_string()),
            driver: mount.driver,
            rw: mount.rw,
            volume_name,
            size_bytes,
        }
    }
}

#[cfg(feature = "client")]
async fn collect_storage_summary(
    docker: &bollard::Docker,
) -> Result<(
    Option<DockerStorageSummary>,
    HashMap<String, u64>,
    Vec<String>,
)> {
    use bollard::query_parameters::{
        ListImagesOptionsBuilder, ListVolumesOptions as VolumeQueryOptions,
    };

    let images = docker
        .list_images(Some(ListImagesOptionsBuilder::default().all(true).build()))
        .await?;

    let mut image_total_bytes = 0u64;
    let mut image_bytes_available = false;
    for summary in &images {
        if summary.size >= 0 {
            image_total_bytes = image_total_bytes.saturating_add(summary.size as u64);
            image_bytes_available = true;
        }
    }

    let volumes_response = docker.list_volumes(None::<VolumeQueryOptions>).await?;

    let mut volume_sizes = HashMap::new();
    let mut volume_total_bytes = 0u64;
    let mut volume_bytes_available = false;
    let mut volume_count = 0usize;

    if let Some(volumes) = volumes_response.volumes {
        volume_count = volumes.len();
        for volume in volumes {
            if let Some(usage) = volume.usage_data {
                if usage.size >= 0 {
                    let size = usage.size as u64;
                    volume_total_bytes = volume_total_bytes.saturating_add(size);
                    volume_bytes_available = true;
                    if !volume.name.is_empty() {
                        volume_sizes.insert(volume.name, size);
                    }
                }
            }
        }
    }

    let mut notes = Vec::new();
    if let Some(warnings) = volumes_response.warnings {
        notes.extend(warnings);
    }

    let storage = DockerStorageSummary {
        image_total_bytes: image_bytes_available.then_some(image_total_bytes),
        image_count: images.len(),
        volume_total_bytes: volume_bytes_available.then_some(volume_total_bytes),
        volume_count,
    };

    Ok((Some(storage), volume_sizes, notes))
}
impl ContainerMetrics {
    fn from_stats(stats: &bollard::models::ContainerStatsResponse) -> Self {
        let cpu_percent = calculate_cpu_percent(stats);
        let (memory_usage_bytes, memory_limit_bytes, memory_percent) = extract_memory_stats(stats);
        let (network_rx_bytes, network_tx_bytes) = aggregate_network_bytes(stats);
        let (block_read_bytes, block_write_bytes) = aggregate_block_io(stats);

        Self {
            cpu_percent,
            memory_usage_bytes,
            memory_limit_bytes,
            memory_percent,
            network_rx_bytes,
            network_tx_bytes,
            block_read_bytes,
            block_write_bytes,
        }
    }
}

#[cfg(feature = "client")]
fn calculate_cpu_percent(stats: &bollard::models::ContainerStatsResponse) -> Option<f64> {
    let cpu = stats.cpu_stats.as_ref()?;
    let precpu = stats.precpu_stats.as_ref()?;
    let cpu_usage = cpu.cpu_usage.as_ref()?;
    let precpu_usage = precpu.cpu_usage.as_ref()?;

    let total_usage = cpu_usage.total_usage?;
    let prev_total = precpu_usage.total_usage?;
    let system_usage = cpu.system_cpu_usage?;
    let prev_system = precpu.system_cpu_usage?;

    let cpu_delta = total_usage.checked_sub(prev_total)?;
    let system_delta = system_usage.checked_sub(prev_system)?;

    if cpu_delta == 0 || system_delta == 0 {
        return Some(0.0);
    }

    let online_cpus = cpu
        .online_cpus
        .map(|value| value as u64)
        .or_else(|| {
            cpu_usage
                .percpu_usage
                .as_ref()
                .map(|usage| usage.len() as u64)
        })
        .filter(|&count| count > 0)
        .unwrap_or(1);

    Some((cpu_delta as f64 / system_delta as f64) * online_cpus as f64 * 100.0)
}

#[cfg(feature = "client")]
fn extract_memory_stats(
    stats: &bollard::models::ContainerStatsResponse,
) -> (Option<u64>, Option<u64>, Option<f64>) {
    let Some(memory) = stats.memory_stats.as_ref() else {
        return (None, None, None);
    };

    let usage = memory.usage;
    let limit = memory.limit;
    let percent = match (usage, limit) {
        (Some(usage), Some(limit)) if limit > 0 => Some((usage as f64 / limit as f64) * 100.0),
        _ => None,
    };

    (usage, limit, percent)
}

#[cfg(feature = "client")]
fn aggregate_network_bytes(
    stats: &bollard::models::ContainerStatsResponse,
) -> (Option<u64>, Option<u64>) {
    let Some(networks) = stats.networks.as_ref() else {
        return (None, None);
    };

    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    let mut has_rx = false;
    let mut has_tx = false;

    for metrics in networks.values() {
        if let Some(rx) = metrics.rx_bytes {
            total_rx = total_rx.saturating_add(rx);
            has_rx = true;
        }
        if let Some(tx) = metrics.tx_bytes {
            total_tx = total_tx.saturating_add(tx);
            has_tx = true;
        }
    }

    (has_rx.then_some(total_rx), has_tx.then_some(total_tx))
}

#[cfg(feature = "client")]
fn aggregate_block_io(
    stats: &bollard::models::ContainerStatsResponse,
) -> (Option<u64>, Option<u64>) {
    let Some(block_io) = stats.blkio_stats.as_ref() else {
        return (None, None);
    };
    let Some(entries) = block_io.io_service_bytes_recursive.as_ref() else {
        return (None, None);
    };

    let mut read_total = 0u64;
    let mut write_total = 0u64;
    let mut has_read = false;
    let mut has_write = false;

    for entry in entries {
        let Some(value) = entry.value else {
            continue;
        };

        match entry.op.as_deref() {
            Some("Read") => {
                read_total = read_total.saturating_add(value);
                has_read = true;
            }
            Some("Write") => {
                write_total = write_total.saturating_add(value);
                has_write = true;
            }
            _ => {}
        }
    }

    (
        has_read.then_some(read_total),
        has_write.then_some(write_total),
    )
}

fn clean_names(raw: Option<Vec<String>>) -> Vec<String> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|name| name.trim_start_matches('/').to_string())
        .collect()
}

#[cfg(feature = "client")]
impl From<bollard::models::ContainerSummary> for ContainerInfo {
    fn from(summary: bollard::models::ContainerSummary) -> Self {
        ContainerInfo {
            id: summary.id.unwrap_or_else(|| "unknown".to_string()),
            names: clean_names(summary.names),
            image: summary.image,
            state: summary.state.map(|state| state.to_string()),
            status: summary.status,
            metrics: None,
            health: None,
            health_failing_streak: None,
            restart_count: None,
            size_rw_bytes: normalize_size(summary.size_rw),
            size_root_fs_bytes: normalize_size(summary.size_root_fs),
            mounts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean_names;

    #[cfg(not(feature = "client"))]
    use super::DockerCollector;
    #[cfg(not(feature = "client"))]
    use vmic_sdk::CollectionContext;
    #[cfg(not(feature = "client"))]
    use vmic_sdk::SectionStatus;

    #[cfg(not(feature = "client"))]
    #[test]
    fn degrade_section_has_expected_summary_without_client() {
        let collector = DockerCollector;
        let section = collector.collect(&CollectionContext::new()).unwrap();
        assert_eq!(section.id, "docker");
        assert!(matches!(section.status, SectionStatus::Degraded));
        assert!(section.notes.is_empty());
    }

    #[test]
    fn clean_names_strips_prefixes() {
        let cleaned = clean_names(Some(vec!["/web".into(), "/api".into()]));
        assert_eq!(cleaned, vec!["web", "api"]);
    }
}

#[cfg(all(test, feature = "client"))]
mod client_feature_tests {
    use super::{ContainerDetails, ContainerInfo, ContainerMetrics, ContainerMountInfo};
    use bollard::models::{
        ContainerBlkioStatEntry, ContainerBlkioStats, ContainerCpuStats, ContainerCpuUsage,
        ContainerMemoryStats, ContainerNetworkStats, ContainerStatsResponse,
    };
    use std::collections::HashMap;

    #[test]
    fn container_metrics_extracts_expected_fields() {
        let stats = ContainerStatsResponse {
            cpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(400),
                    percpu_usage: Some(vec![200, 200]),
                    ..Default::default()
                }),
                system_cpu_usage: Some(1_000),
                online_cpus: Some(2),
                ..Default::default()
            }),
            precpu_stats: Some(ContainerCpuStats {
                cpu_usage: Some(ContainerCpuUsage {
                    total_usage: Some(100),
                    ..Default::default()
                }),
                system_cpu_usage: Some(400),
                online_cpus: Some(2),
                ..Default::default()
            }),
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(512),
                limit: Some(1_024),
                ..Default::default()
            }),
            networks: Some(HashMap::from([(
                "eth0".to_string(),
                ContainerNetworkStats {
                    rx_bytes: Some(100),
                    tx_bytes: Some(200),
                    ..Default::default()
                },
            )])),
            blkio_stats: Some(ContainerBlkioStats {
                io_service_bytes_recursive: Some(vec![
                    ContainerBlkioStatEntry {
                        major: None,
                        minor: None,
                        op: Some("Read".to_string()),
                        value: Some(1_000),
                    },
                    ContainerBlkioStatEntry {
                        major: None,
                        minor: None,
                        op: Some("Write".to_string()),
                        value: Some(2_000),
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let metrics = ContainerMetrics::from_stats(&stats);

        let cpu = metrics.cpu_percent.expect("cpu");
        assert!((cpu - 100.0).abs() < 1e-6);
        assert_eq!(metrics.memory_usage_bytes, Some(512));
        assert_eq!(metrics.memory_limit_bytes, Some(1_024));
        let mem_percent = metrics.memory_percent.expect("memory percent");
        assert!((mem_percent - 50.0).abs() < 1e-6);
        assert_eq!(metrics.network_rx_bytes, Some(100));
        assert_eq!(metrics.network_tx_bytes, Some(200));
        assert_eq!(metrics.block_read_bytes, Some(1_000));
        assert_eq!(metrics.block_write_bytes, Some(2_000));
    }

    #[test]
    fn normalize_size_handles_negative_values() {
        assert_eq!(super::normalize_size(Some(-1)), None);
        assert_eq!(super::normalize_size(Some(0)), Some(0));
        assert_eq!(super::normalize_size(Some(2048)), Some(2048));
    }

    #[test]
    fn apply_details_populates_fields() {
        let mut info = ContainerInfo {
            id: "abc".into(),
            names: vec!["app".into()],
            image: None,
            state: None,
            status: None,
            metrics: None,
            health: None,
            health_failing_streak: None,
            restart_count: None,
            size_rw_bytes: None,
            size_root_fs_bytes: None,
            mounts: Vec::new(),
        };

        let details = ContainerDetails {
            health_status: Some("unhealthy".into()),
            health_failing_streak: Some(3),
            restart_count: Some(4),
            size_rw_bytes: Some(1_024),
            size_root_fs_bytes: Some(4_096),
            mounts: vec![ContainerMountInfo::default()],
        };

        info.apply_details(details);

        assert_eq!(info.health.as_deref(), Some("unhealthy"));
        assert_eq!(info.health_failing_streak, Some(3));
        assert_eq!(info.restart_count, Some(4));
        assert_eq!(info.size_rw_bytes, Some(1_024));
        assert_eq!(info.size_root_fs_bytes, Some(4_096));
        assert_eq!(info.mounts.len(), 1);
    }
}
