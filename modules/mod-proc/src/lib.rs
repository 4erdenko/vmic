use anyhow::{Context as _, Result};
use procfs::{Current, LoadAverage, Meminfo};
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct ProcCollector;

impl Collector for ProcCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "proc",
            title: "Процессы и ресурсы",
            description: "Обзор /proc: нагрузка и память",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let snapshot = build_snapshot().context("не удалось получить показатели /proc")?;
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
    total_memory_kb: Option<u64>,
    available_memory_kb: Option<u64>,
    swap_total_kb: Option<u64>,
    swap_free_kb: Option<u64>,
}

fn build_snapshot() -> Result<ProcSnapshot> {
    let loadavg = LoadAverage::current()
        .ok()
        .map(|l| (l.one, l.five, l.fifteen));
    let meminfo = Meminfo::current().ok();

    Ok(ProcSnapshot {
        loadavg,
        total_memory_kb: meminfo.as_ref().map(|m| m.mem_total),
        available_memory_kb: meminfo.as_ref().and_then(|m| m.mem_available),
        swap_total_kb: meminfo.as_ref().map(|m| m.swap_total),
        swap_free_kb: meminfo.as_ref().map(|m| m.swap_free),
    })
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
        "memory_kb": {
            "total": snapshot.total_memory_kb,
            "available": snapshot.available_memory_kb,
        },
        "swap_kb": {
            "total": snapshot.swap_total_kb,
            "free": snapshot.swap_free_kb,
        }
    });

    let mut section = Section::success("proc", "Процессы и ресурсы", body);
    section.summary = Some(snapshot.summary());
    section
}

impl ProcSnapshot {
    fn summary(&self) -> String {
        match self.loadavg {
            Some((one, _, _)) => format!("LoadAvg 1m: {:.2}", one),
            None => "LoadAvg недоступен".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_with_loadavg() {
        let snapshot = ProcSnapshot {
            loadavg: Some((0.5, 0.4, 0.3)),
            total_memory_kb: Some(1024),
            available_memory_kb: Some(512),
            swap_total_kb: Some(256),
            swap_free_kb: Some(128),
        };

        assert_eq!(snapshot.summary(), "LoadAvg 1m: 0.50");
    }

    #[test]
    fn section_contains_memory_totals() {
        let snapshot = ProcSnapshot {
            loadavg: None,
            total_memory_kb: Some(2048),
            available_memory_kb: Some(1024),
            swap_total_kb: None,
            swap_free_kb: None,
        };

        let section = section_from_snapshot(&snapshot);
        let mem = section.body.get("memory_kb").unwrap();
        assert_eq!(mem.get("total").and_then(|v| v.as_u64()), Some(2048));
    }
}
