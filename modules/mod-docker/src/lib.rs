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
                    });
                    let mut section = Section::success("docker", "Docker Containers", body);
                    section.summary = Some(format!(
                        "{} containers discovered",
                        snapshot.containers.len()
                    ));
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ContainerInfo {
    id: String,
    names: Vec<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DockerSnapshot {
    engine: Option<EngineInfo>,
    containers: Vec<ContainerInfo>,
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

        Ok(DockerSnapshot {
            engine: Some(engine),
            containers: containers.into_iter().map(ContainerInfo::from).collect(),
        })
    })
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmic_sdk::SectionStatus;

    #[test]
    fn degrade_section_has_expected_summary_without_client() {
        let collector = DockerCollector;
        let section = collector.collect(&CollectionContext::new()).unwrap();
        assert_eq!(section.id, "docker");
        assert!(matches!(section.status, SectionStatus::Degraded));
    }

    #[test]
    fn clean_names_strips_prefixes() {
        let cleaned = super::clean_names(Some(vec!["/web".into(), "/api".into()]));
        assert_eq!(cleaned, vec!["web", "api"]);
    }
}
