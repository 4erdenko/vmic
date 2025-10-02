use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use vmic_sdk::{self, CollectionContext, Section};

use crate::health::{HealthDigest, build_health_digest};
pub use health::{DigestThresholds, Severity};

pub use vmic_sdk::{CollectionContext as Context, SectionStatus};

#[derive(Debug, Serialize)]
pub struct ReportMetadata {
    pub generated_at: String,
    pub sections: usize,
}

impl ReportMetadata {
    pub fn generated_at_utc(&self) -> Option<DateTime<Utc>> {
        let seconds = self.generated_at.parse::<i64>().ok()?;
        DateTime::<Utc>::from_timestamp(seconds, 0)
    }

    pub fn generated_at_iso8601(&self) -> String {
        self.generated_at_utc()
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub metadata: ReportMetadata,
    pub sections: Vec<Section>,
    pub health_digest: HealthDigest,
}

impl Report {
    pub fn new(sections: Vec<Section>) -> Self {
        Self::with_digest_config(sections, DigestThresholds::default())
    }

    pub fn with_digest_config(sections: Vec<Section>, thresholds: DigestThresholds) -> Self {
        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".to_string());

        let count = sections.len();

        let health_digest = build_health_digest(&sections, &thresholds);

        Self {
            metadata: ReportMetadata {
                generated_at,
                sections: count,
            },
            sections,
            health_digest,
        }
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "metadata": {
                "generated_at": self.metadata.generated_at,
                "sections": self.metadata.sections,
                "health_digest": self.health_digest,
            },
            "sections": self.sections,
        })
    }

    pub fn to_markdown(&self) -> Result<String> {
        render::render_markdown(self).map_err(Into::into)
    }

    pub fn to_html(&self) -> Result<String> {
        render::render_html(self).map_err(Into::into)
    }
}

fn collect_sections(ctx: &CollectionContext) -> Vec<Section> {
    let mut sections = Vec::new();

    for entry in vmic_sdk::iter_registered_collectors() {
        let collector = (entry.constructor)();
        let metadata = collector.metadata();

        match collector.collect(ctx) {
            Ok(section) => sections.push(section),
            Err(error) => sections.push(Section::error(
                metadata.id,
                metadata.title,
                error.to_string(),
            )),
        }
    }

    sections
}

pub fn collect_report(ctx: &CollectionContext) -> Report {
    Report::new(collect_sections(ctx))
}

pub fn collect_report_with_digest(ctx: &CollectionContext, thresholds: DigestThresholds) -> Report {
    Report::with_digest_config(collect_sections(ctx), thresholds)
}

mod health {
    use super::{Section, SectionStatus};
    use anyhow::{Result, anyhow};
    use serde::Serialize;
    use serde_json::Value;

    #[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "lowercase")]
    pub enum Severity {
        Info,
        Warning,
        Critical,
    }

    impl Severity {
        pub fn as_str(&self) -> &'static str {
            match self {
                Severity::Info => "info",
                Severity::Warning => "warning",
                Severity::Critical => "critical",
            }
        }

        pub fn display_label(&self) -> &'static str {
            match self {
                Severity::Info => "Info",
                Severity::Warning => "Warning",
                Severity::Critical => "Critical",
            }
        }
    }

    impl Default for Severity {
        fn default() -> Self {
            Severity::Info
        }
    }

    #[derive(Debug, Clone, Serialize, Default)]
    pub struct HealthDigest {
        pub overall: Severity,
        pub findings: Vec<CriticalFinding>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct CriticalFinding {
        pub source_id: String,
        pub source_title: String,
        pub severity: Severity,
        pub message: String,
    }

    impl CriticalFinding {
        fn new(section: &Section, severity: Severity, message: String) -> Self {
            Self {
                source_id: section.id.to_string(),
                source_title: section.title.to_string(),
                severity,
                message,
            }
        }
    }

    #[derive(Debug, Clone, Copy, Serialize)]
    pub struct DigestThresholds {
        pub disk_warning: f64,
        pub disk_critical: f64,
        pub memory_warning: f64,
        pub memory_critical: f64,
    }

    impl Default for DigestThresholds {
        fn default() -> Self {
            Self {
                disk_warning: 0.90,
                disk_critical: 0.95,
                memory_warning: 0.10,
                memory_critical: 0.05,
            }
        }
    }

    impl DigestThresholds {
        pub fn validate(&self) -> Result<()> {
            for (name, value) in [
                ("disk_warning", self.disk_warning),
                ("disk_critical", self.disk_critical),
                ("memory_warning", self.memory_warning),
                ("memory_critical", self.memory_critical),
            ] {
                if !(0.0..=1.0).contains(&value) {
                    return Err(anyhow!("{} must be between 0 and 1", name));
                }
            }

            if self.disk_warning > self.disk_critical {
                return Err(anyhow!(
                    "disk_warning ({:.2}%) must be <= disk_critical ({:.2}%)",
                    self.disk_warning * 100.0,
                    self.disk_critical * 100.0
                ));
            }

            if self.memory_warning < self.memory_critical {
                return Err(anyhow!(
                    "memory_warning ({:.2}%) must be >= memory_critical ({:.2}%)",
                    self.memory_warning * 100.0,
                    self.memory_critical * 100.0
                ));
            }

            Ok(())
        }
    }

    pub fn build_health_digest(
        sections: &[Section],
        thresholds: &DigestThresholds,
    ) -> HealthDigest {
        let mut findings: Vec<CriticalFinding> = Vec::new();

        for section in sections {
            match section.status {
                SectionStatus::Success => {}
                SectionStatus::Degraded => {
                    let message = section
                        .summary
                        .clone()
                        .unwrap_or_else(|| "Collector reported a degraded state".to_string());
                    findings.push(CriticalFinding::new(section, Severity::Warning, message));
                }
                SectionStatus::Error => {
                    let message = section
                        .summary
                        .clone()
                        .unwrap_or_else(|| "Collector failed".to_string());
                    findings.push(CriticalFinding::new(section, Severity::Critical, message));
                }
            }

            collect_storage_alerts(section, thresholds, &mut findings);
            collect_proc_alerts(section, thresholds, &mut findings);
        }

        let overall = findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Info);

        HealthDigest { overall, findings }
    }

    fn collect_storage_alerts(
        section: &Section,
        thresholds: &DigestThresholds,
        findings: &mut Vec<CriticalFinding>,
    ) {
        if section.id != "storage" {
            return;
        }

        let mounts = section
            .body
            .get("mounts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for mount in mounts {
            let Some(point) = mount.get("mount_point").and_then(Value::as_str) else {
                continue;
            };
            let Some(ratio) = mount.get("usage_ratio").and_then(Value::as_f64) else {
                continue;
            };

            let severity = if ratio >= thresholds.disk_critical {
                Some(Severity::Critical)
            } else if ratio >= thresholds.disk_warning {
                Some(Severity::Warning)
            } else {
                None
            };

            if let Some(severity) = severity {
                let message = format!("Mount {} at {:.1}% capacity", point, ratio * 100.0);
                findings.push(CriticalFinding::new(section, severity, message));
            }
        }
    }

    fn collect_proc_alerts(
        section: &Section,
        thresholds: &DigestThresholds,
        findings: &mut Vec<CriticalFinding>,
    ) {
        if section.id != "proc" {
            return;
        }

        let Some(memory) = section.body.get("memory_kb") else {
            return;
        };

        let total = memory.get("total").and_then(Value::as_u64).unwrap_or(0);
        let available = memory.get("available").and_then(Value::as_u64).unwrap_or(0);

        if total == 0 {
            return;
        }

        let ratio = available as f64 / total as f64;

        let severity = if ratio <= thresholds.memory_critical {
            Some(Severity::Critical)
        } else if ratio <= thresholds.memory_warning {
            Some(Severity::Warning)
        } else {
            None
        };

        if let Some(severity) = severity {
            let message = format!(
                "Available memory {:.1}% of total ({} MiB free)",
                ratio * 100.0,
                available / 1024
            );
            findings.push(CriticalFinding::new(section, severity, message));
        }
    }
}

mod render {
    use askama::Template;

    use crate::filters;

    use super::Report;

    #[derive(Template)]
    #[template(path = "report.md", escape = "none")]
    struct MarkdownReport<'a> {
        report: &'a Report,
    }

    #[derive(Template)]
    #[template(path = "report.html")]
    struct HtmlReport<'a> {
        report: &'a Report,
    }

    pub fn render_markdown(report: &Report) -> askama::Result<String> {
        MarkdownReport { report }.render()
    }

    pub fn render_html(report: &Report) -> askama::Result<String> {
        HtmlReport { report }.render()
    }
}

pub mod filters {
    use askama::{Error, Result, Values};
    use serde_json::Value;

    pub fn json_pretty(value: &Value, _args: &dyn Values) -> Result<String> {
        serde_json::to_string_pretty(value).map_err(|err| Error::Custom(Box::new(err)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vmic_sdk::SectionStatus;

    // Link modules so their collectors register during tests.
    use mod_containers as _;
    use mod_cron as _;
    use mod_docker as _;
    use mod_journal as _;
    use mod_os as _;
    use mod_proc as _;
    use mod_sar as _;
    use mod_services as _;
    use mod_users as _;

    #[test]
    fn collect_report_returns_sections() {
        let ctx = Context::new();
        let report = collect_report(&ctx);
        assert!(!report.sections.is_empty());
        assert!(report.sections.iter().any(|s| s.id == "os"));
        assert!(
            report
                .sections
                .iter()
                .all(|s| !matches!(s.status, SectionStatus::Error))
        );
        assert_eq!(report.metadata.sections, report.sections.len());
        let expected_overall = report
            .health_digest
            .findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Info);
        assert_eq!(report.health_digest.overall, expected_overall);
    }

    #[test]
    fn markdown_render_contains_section_title() {
        let ctx = Context::new();
        let report = collect_report(&ctx);
        let md = report.to_markdown().expect("markdown render");
        assert!(md.contains("# System Report"));
        assert!(md.contains("Critical Health Digest"));
    }

    #[test]
    fn html_render_contains_structure() {
        let ctx = Context::new();
        let report = collect_report(&ctx);
        let html = report.to_html().expect("html render");
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("System Report"));
        assert!(html.contains("class=\"card status-"));
        assert!(html.contains("Overall Status"));
    }

    #[test]
    fn metadata_provides_iso8601_timestamp() {
        let ctx = Context::new();
        let report = collect_report(&ctx);
        let iso = report.metadata.generated_at_iso8601();
        assert!(iso.contains('T'));
        assert!(iso.ends_with("+00:00"));
    }

    #[test]
    fn digest_highlights_degraded_sections() {
        let degraded = Section::degraded("demo", "Demo", "something off".to_string(), json!({}));
        let report = Report::new(vec![degraded]);
        assert_eq!(report.health_digest.overall, Severity::Warning);
        assert_eq!(report.health_digest.findings.len(), 1);
        assert!(
            report.health_digest.findings[0]
                .message
                .contains("something off")
        );
    }

    #[test]
    fn digest_flags_high_disk_usage() {
        let storage = Section::success(
            "storage",
            "Storage Overview",
            json!({
                "mounts": [
                    {
                        "mount_point": "/data",
                        "fs_type": "ext4",
                        "total_bytes": 100,
                        "used_bytes": 95,
                        "available_bytes": 5,
                        "usage_ratio": 0.95
                    }
                ],
                "totals": json!({})
            }),
        );
        let report = Report::new(vec![storage]);
        assert_eq!(report.health_digest.overall, Severity::Critical);
        assert!(
            report
                .health_digest
                .findings
                .iter()
                .any(|f| f.source_id == "storage" && f.severity == Severity::Critical)
        );
    }

    #[test]
    fn custom_thresholds_trigger_warning() {
        let storage = Section::success(
            "storage",
            "Storage Overview",
            json!({
                "mounts": [
                    {
                        "mount_point": "/data",
                        "fs_type": "ext4",
                        "total_bytes": 100,
                        "used_bytes": 85,
                        "available_bytes": 15,
                        "usage_ratio": 0.85
                    }
                ],
                "totals": json!({})
            }),
        );

        let thresholds = DigestThresholds {
            disk_warning: 0.80,
            disk_critical: 0.90,
            ..DigestThresholds::default()
        };

        let report = Report::with_digest_config(vec![storage], thresholds);
        assert_eq!(report.health_digest.overall, Severity::Warning);
        assert!(
            report
                .health_digest
                .findings
                .iter()
                .any(|f| f.source_id == "storage" && f.severity == Severity::Warning)
        );
    }
}
