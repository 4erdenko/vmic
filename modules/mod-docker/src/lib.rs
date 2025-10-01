use anyhow::Result;
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct DockerCollector;

impl Collector for DockerCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "docker",
            title: "Контейнеры Docker",
            description: "Состояние Docker Engine и контейнеров",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        #[cfg(feature = "client")]
        {
            let body = json!({
                "engine": "недоступно",
                "containers": Vec::<serde_json::Value>::new(),
            });

            return Ok(Section::degraded(
                "docker",
                "Контейнеры Docker",
                "Интеграция с Docker ещё не реализована".to_string(),
                body,
            ));
        }

        #[cfg(not(feature = "client"))]
        {
            let body = json!({
                "containers": Vec::<serde_json::Value>::new(),
            });

            Ok(Section::degraded(
                "docker",
                "Контейнеры Docker",
                "Фича docker отключена или зависимости недоступны".to_string(),
                body,
            ))
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(DockerCollector)
}

register_collector!(create_collector);

#[cfg(test)]
mod tests {
    use super::*;
    use vmic_sdk::CollectionContext;

    #[test]
    fn docker_collector_degraded() {
        let collector = DockerCollector;
        let section = collector.collect(&CollectionContext::new()).unwrap();
        assert_eq!(section.id, "docker");
        assert!(matches!(section.status, vmic_sdk::SectionStatus::Degraded));
    }
}
