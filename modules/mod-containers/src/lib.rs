use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::process::Command;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct ContainersCollector;

impl Collector for ContainersCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "containers",
            title: "Alternative Containers",
            description: "Podman and containerd runtimes",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let snapshot = build_snapshot()?;
        Ok(section_from_snapshot(&snapshot))
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(ContainersCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RuntimeInfo {
    name: String,
    version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainersSnapshot {
    runtimes: Vec<RuntimeInfo>,
}

impl ContainersSnapshot {
    fn summary(&self) -> String {
        match self.runtimes.len() {
            0 => "No alternative container runtimes detected".to_string(),
            count => format!("{} runtime(s) detected", count),
        }
    }
}

fn build_snapshot() -> Result<ContainersSnapshot> {
    let mut runtimes = Vec::new();

    if let Some(info) = detect_runtime("podman", &["--version"]) {
        runtimes.push(info);
    }

    if let Some(info) = detect_runtime("nerdctl", &["--version"]) {
        runtimes.push(info);
    }

    if let Some(info) = detect_runtime("ctr", &["version"]) {
        runtimes.push(info);
    }

    Ok(ContainersSnapshot { runtimes })
}

fn detect_runtime(command: &str, args: &[&str]) -> Option<RuntimeInfo> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = extract_version(stdout.trim());
    Some(RuntimeInfo {
        name: command.to_string(),
        version,
    })
}

fn extract_version(line: &str) -> Option<String> {
    let first_line = line.lines().next().unwrap_or(line).trim();
    if first_line.is_empty() {
        None
    } else {
        Some(first_line.to_string())
    }
}

fn section_from_snapshot(snapshot: &ContainersSnapshot) -> Section {
    let body = json!({
        "runtimes": snapshot.runtimes,
    });
    let mut section = Section::success("containers", "Alternative Containers", body);
    section.summary = Some(snapshot.summary());
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_returns_first_line() {
        let version = extract_version("podman version 4.5.0\nextra").expect("version");
        assert_eq!(version, "podman version 4.5.0");
    }

    #[test]
    fn snapshot_summary_reports_count() {
        let snapshot = ContainersSnapshot {
            runtimes: vec![RuntimeInfo {
                name: "podman".into(),
                version: Some("podman version".into()),
            }],
        };

        assert_eq!(snapshot.summary(), "1 runtime(s) detected");
    }
}
