use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::process::Command;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct ServicesCollector;

impl Collector for ServicesCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "services",
            title: "System Services",
            description: "systemd services status",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match gather_snapshot() {
            Ok(snapshot) => Ok(section_from_snapshot(&snapshot)),
            Err(error) => Ok(Section::degraded(
                "services",
                "System Services",
                error.to_string(),
                json!({
                    "running": Vec::<serde_json::Value>::new(),
                    "failed": Vec::<serde_json::Value>::new(),
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(ServicesCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ServiceInfo {
    unit: String,
    load: String,
    active: String,
    sub: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServicesSnapshot {
    running: Vec<ServiceInfo>,
    failed: Vec<ServiceInfo>,
}

impl ServicesSnapshot {
    fn summary(&self) -> String {
        format!(
            "{} running, {} failed services",
            self.running.len(),
            self.failed.len()
        )
    }
}

fn gather_snapshot() -> Result<ServicesSnapshot> {
    let running_output = run_systemctl(&[
        "list-units",
        "--type=service",
        "--state=running",
        "--no-legend",
        "--no-pager",
    ])?;
    let failed_output = run_systemctl(&[
        "list-units",
        "--type=service",
        "--state=failed",
        "--no-legend",
        "--no-pager",
    ])?;

    Ok(ServicesSnapshot {
        running: parse_systemctl_units(&running_output),
        failed: parse_systemctl_units(&failed_output),
    })
}

fn run_systemctl(args: &[&str]) -> Result<String> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute systemctl {}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl {}: {}", args.join(" "), stderr.trim())
    }
}

fn parse_systemctl_units(output: &str) -> Vec<ServiceInfo> {
    output
        .lines()
        .filter_map(|line| parse_systemctl_line(line).ok())
        .collect()
}

fn parse_systemctl_line(line: &str) -> Result<ServiceInfo> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("ignored");
    }

    let mut parts = trimmed.split_whitespace();
    let unit = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing unit"))?;
    let load = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing load"))?;
    let active = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing active"))?;
    let sub = parts.next().ok_or_else(|| anyhow::anyhow!("missing sub"))?;
    let description = parts.collect::<Vec<_>>().join(" ");

    Ok(ServiceInfo {
        unit: unit.to_string(),
        load: load.to_string(),
        active: active.to_string(),
        sub: sub.to_string(),
        description,
    })
}

fn section_from_snapshot(snapshot: &ServicesSnapshot) -> Section {
    let body = json!({
        "running": snapshot.running,
        "failed": snapshot.failed,
    });
    let mut section = Section::success("services", "System Services", body);
    section.summary = Some(snapshot.summary());
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_systemctl_line_extracts_description() {
        let line = "cron.service loaded active running Regular background program";
        let info = parse_systemctl_line(line).expect("parsed service");
        assert_eq!(info.unit, "cron.service");
        assert_eq!(info.load, "loaded");
        assert_eq!(info.active, "active");
        assert!(info.description.contains("Regular"));
    }

    #[test]
    fn snapshot_summary_counts_services() {
        let snapshot = ServicesSnapshot {
            running: vec![ServiceInfo {
                unit: "cron.service".into(),
                load: "loaded".into(),
                active: "active".into(),
                sub: "running".into(),
                description: "Cron".into(),
            }],
            failed: vec![ServiceInfo {
                unit: "failed.service".into(),
                load: "loaded".into(),
                active: "failed".into(),
                sub: "failed".into(),
                description: "Broken".into(),
            }],
        };

        assert_eq!(snapshot.summary(), "1 running, 1 failed services");
    }
}
