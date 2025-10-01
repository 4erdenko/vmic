use anyhow::Result;
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct JournalCollector;

impl Collector for JournalCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "journal",
            title: "systemd journal",
            description: "Последние события из journald",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let body = json!({
            "source": "journalctl -o json",
            "entries": Vec::<serde_json::Value>::new(),
        });

        Ok(Section::degraded(
            "journal",
            "systemd journal",
            "Чтение журнала ещё не реализовано".to_string(),
            body,
        ))
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(JournalCollector)
}

register_collector!(create_collector);

#[cfg(test)]
mod tests {
    use super::*;
    use vmic_sdk::CollectionContext;

    #[test]
    fn collect_returns_degraded_section() {
        let collector = JournalCollector;
        let section = collector.collect(&CollectionContext::new()).unwrap();
        assert_eq!(section.id, "journal");
        assert!(matches!(section.status, vmic_sdk::SectionStatus::Degraded));
    }
}
