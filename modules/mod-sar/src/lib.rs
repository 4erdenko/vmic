use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::process::Command;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct SarCollector;

impl Collector for SarCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "sar",
            title: "Sysstat Metrics",
            description: "CPU averages from sar",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match gather_snapshot() {
            Ok(snapshot) => Ok(section_from_snapshot(&snapshot)),
            Err(error) => Ok(Section::degraded(
                "sar",
                "Sysstat Metrics",
                error.to_string(),
                json!({
                    "cpu": serde_json::Value::Null,
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(SarCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, Serialize, PartialEq)]
struct CpuAverages {
    user: f64,
    nice: f64,
    system: f64,
    iowait: f64,
    steal: f64,
    idle: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct SarSnapshot {
    cpu: CpuAverages,
}

impl SarSnapshot {
    fn summary(&self) -> String {
        format!(
            "CPU avg: user {:.1}%, system {:.1}%, idle {:.1}%",
            self.cpu.user, self.cpu.system, self.cpu.idle
        )
    }
}

fn gather_snapshot() -> Result<SarSnapshot> {
    let output = run_sar_command()?;
    let averages = parse_sar_cpu(&output).context("failed to parse sar output")?;
    Ok(SarSnapshot { cpu: averages })
}

fn run_sar_command() -> Result<String> {
    let output = Command::new("sar")
        .args(["-u", "1", "1"])
        .output()
        .with_context(|| "failed to execute sar -u 1 1")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sar command failed: {}", stderr.trim())
    }
}

fn parse_sar_cpu(output: &str) -> Option<CpuAverages> {
    output
        .lines()
        .find(|line| line.trim_start().starts_with("Average:"))
        .and_then(|line| parse_average_line(line).ok())
}

fn parse_average_line(line: &str) -> Result<CpuAverages> {
    let mut parts = line.split_whitespace();
    parts.next(); // Average:
    parts.next(); // CPU or all

    let user = parse_percentage(parts.next(), "user")?;
    let nice = parse_percentage(parts.next(), "nice")?;
    let system = parse_percentage(parts.next(), "system")?;
    let iowait = parse_percentage(parts.next(), "iowait")?;
    let steal = parse_percentage(parts.next(), "steal")?;
    let idle = parse_percentage(parts.next(), "idle")?;

    Ok(CpuAverages {
        user,
        nice,
        system,
        iowait,
        steal,
        idle,
    })
}

fn parse_percentage(value: Option<&str>, field: &str) -> Result<f64> {
    let value = value.ok_or_else(|| anyhow::anyhow!("missing {}", field))?;
    value
        .replace(',', ".")
        .parse::<f64>()
        .with_context(|| format!("invalid {} percentage", field))
}

fn section_from_snapshot(snapshot: &SarSnapshot) -> Section {
    let body = json!({
        "cpu": snapshot.cpu,
    });
    let mut section = Section::success("sar", "Sysstat Metrics", body);
    section.summary = Some(snapshot.summary());
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Linux 6.9.0 (localhost)  01/01/2024\n\n12:00:00 AM     CPU     %user     %nice   %system   %iowait    %steal     %idle\n12:00:01 AM     all      1.00      0.00      2.00      0.00      0.00     97.00\nAverage:        all      0.80      0.00      1.50      0.10      0.00     97.60\n";

    #[test]
    fn parse_sar_cpu_finds_average_line() {
        let averages = parse_sar_cpu(SAMPLE).expect("averages");
        assert_eq!(averages.user, 0.8);
        assert_eq!(averages.system, 1.5);
        assert_eq!(averages.idle, 97.6);
    }

    #[test]
    fn snapshot_summary_formats_values() {
        let snapshot = SarSnapshot {
            cpu: CpuAverages {
                user: 1.2,
                nice: 0.0,
                system: 0.8,
                iowait: 0.1,
                steal: 0.0,
                idle: 97.9,
            },
        };

        assert!(snapshot.summary().contains("user 1.2%"));
    }
}
