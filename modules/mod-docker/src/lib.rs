use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
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
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct DockerSnapshot {
    engine: Option<EngineInfo>,
    containers: Vec<ContainerInfo>,
    notes: Vec<String>,
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
}

#[cfg(feature = "client")]
fn collect_docker_snapshot() -> Result<DockerSnapshot> {
    use bollard::Docker;
    use bollard::query_parameters::ListContainersOptionsBuilder;
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

        let options = ListContainersOptionsBuilder::default().all(true).build();

        let containers = docker
            .list_containers(Some(options))
            .await
            .context("failed to list containers")?;

        let engine = EngineInfo {
            version: version.version,
            api_version: version.api_version,
        };

        Ok(collect_containers_with_metrics(&docker, containers, engine).await)
    })
}

#[cfg(feature = "client")]
const METRICS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(feature = "client")]
async fn collect_containers_with_metrics(
    docker: &bollard::Docker,
    containers: Vec<bollard::models::ContainerSummary>,
    engine: EngineInfo,
) -> DockerSnapshot {
    use bollard::query_parameters::StatsOptionsBuilder;

    let stats_options = StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();

    let mut enriched = Vec::with_capacity(containers.len());
    let mut notes = Vec::new();

    for summary in containers {
        let info = ContainerInfo::from(summary);
        match fetch_container_metrics(docker, &info.id, &stats_options).await {
            Ok(metrics) => enriched.push(info.with_metrics(Some(metrics))),
            Err(error) => {
                notes.push(format!(
                    "Failed to collect stats for container {}: {}",
                    info.id, error
                ));
                enriched.push(info);
            }
        }
    }

    DockerSnapshot {
        engine: Some(engine),
        containers: enriched,
        notes,
    }
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
    use super::ContainerMetrics;
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
}
