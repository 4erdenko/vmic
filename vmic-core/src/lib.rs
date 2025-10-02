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
    use std::cmp::Ordering;

    use super::{Report, SectionStatus};
    use serde_json::Value;

    #[derive(Template)]
    #[template(path = "report.md", escape = "none")]
    struct MarkdownReport<'a> {
        report: &'a Report,
    }

    #[derive(Template)]
    #[template(path = "report.html")]
    struct HtmlReport<'a> {
        report: &'a Report,
        sections: Vec<SectionView>,
    }

    pub fn render_markdown(report: &Report) -> askama::Result<String> {
        MarkdownReport { report }.render()
    }

    pub fn render_html(report: &Report) -> askama::Result<String> {
        HtmlReport {
            report,
            sections: build_section_views(report),
        }
        .render()
    }

    #[derive(Debug)]
    struct SectionView {
        title: String,
        status_class: &'static str,
        status_label: String,
        summary: Option<String>,
        notes: Vec<String>,
        key_values: Vec<KeyValue>,
        tables: Vec<TableView>,
        lists: Vec<ListView>,
        paragraph: Option<String>,
        has_key_values: bool,
        has_tables: bool,
        has_lists: bool,
        has_notes: bool,
    }

    impl SectionView {
        fn new(section: &super::Section) -> Self {
            Self {
                title: section.title.to_string(),
                status_class: status_class(&section.status),
                status_label: status_label(&section.status),
                summary: section.summary.clone(),
                notes: section.notes.clone(),
                key_values: Vec::new(),
                tables: Vec::new(),
                lists: Vec::new(),
                paragraph: None,
                has_key_values: false,
                has_tables: false,
                has_lists: false,
                has_notes: !section.notes.is_empty(),
            }
        }

        fn add_kv<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
            self.key_values.push(KeyValue {
                key: key.into(),
                value: value.into(),
            });
        }

        fn add_table(&mut self, table: TableView) {
            if !table.rows.is_empty() {
                self.tables.push(table);
            }
        }

        fn add_list(&mut self, list: ListView) {
            if !list.items.is_empty() {
                self.lists.push(list);
            }
        }

        fn finalize(&mut self) {
            self.has_key_values = !self.key_values.is_empty();
            self.has_tables = !self.tables.is_empty();
            self.has_lists = !self.lists.is_empty();
        }
    }

    #[derive(Debug)]
    struct KeyValue {
        key: String,
        value: String,
    }

    #[derive(Debug)]
    struct TableView {
        title: Option<String>,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    }

    #[derive(Debug)]
    struct ListView {
        title: Option<String>,
        items: Vec<String>,
    }

    fn build_section_views(report: &Report) -> Vec<SectionView> {
        report
            .sections
            .iter()
            .map(|section| {
                let mut view = SectionView::new(section);
                populate_section(&mut view, section.id, &section.body);
                view.finalize();
                view
            })
            .collect()
    }

    fn populate_section(view: &mut SectionView, id: &str, body: &Value) {
        match id {
            "os" => populate_os(view, body),
            "proc" => populate_proc(view, body),
            "storage" => populate_storage(view, body),
            "services" => populate_services(view, body),
            "network" => populate_network(view, body),
            "journal" => populate_journal(view, body),
            "cron" => populate_cron(view, body),
            "docker" => populate_docker(view, body),
            "containers" => populate_containers(view, body),
            "users" => populate_users(view, body),
            _ => populate_generic(view, body),
        }
    }

    fn populate_os(view: &mut SectionView, body: &Value) {
        if let Some(os_release) = body.get("os_release").and_then(Value::as_object) {
            if let Some(pretty) = os_release.get("pretty_name").and_then(Value::as_str) {
                view.add_kv("Distribution", pretty);
            } else if let Some(name) = os_release.get("name").and_then(Value::as_str) {
                view.add_kv("Distribution", name);
            }
            if let Some(version) = os_release.get("version").and_then(Value::as_str) {
                view.add_kv("Version", version);
            }
            if let Some(id_like) = os_release.get("id_like").and_then(Value::as_array) {
                let values: Vec<&str> = id_like.iter().filter_map(Value::as_str).collect();
                if !values.is_empty() {
                    view.add_kv("ID Like", values.join(", "));
                }
            }
        }

        if let Some(kernel) = body.get("kernel").and_then(Value::as_object) {
            if let Some(release) = kernel.get("release").and_then(Value::as_str) {
                view.add_kv("Kernel Release", release);
            }
            if let Some(version) = kernel.get("version").and_then(Value::as_str) {
                view.add_kv("Kernel Version", version);
            }
            if let Some(machine) = kernel.get("machine").and_then(Value::as_str) {
                view.add_kv("Architecture", machine);
            }
        }
    }

    fn populate_proc(view: &mut SectionView, body: &Value) {
        if let Some(load) = body.get("loadavg").and_then(Value::as_object) {
            if let Some(one) = load.get("one").and_then(Value::as_f64) {
                view.add_kv("Load (1m)", format!("{:.2}", one));
            }
            if let Some(five) = load.get("five").and_then(Value::as_f64) {
                view.add_kv("Load (5m)", format!("{:.2}", five));
            }
            if let Some(fifteen) = load.get("fifteen").and_then(Value::as_f64) {
                view.add_kv("Load (15m)", format!("{:.2}", fifteen));
            }
        }

        if let Some(memory) = body.get("memory_kb").and_then(Value::as_object) {
            if let Some(total) = memory.get("total").and_then(Value::as_u64) {
                view.add_kv("Memory Total", format_bytes(total * 1024));
            }
            if let (Some(total), Some(available)) = (
                memory.get("total").and_then(Value::as_u64),
                memory.get("available").and_then(Value::as_u64),
            ) {
                let ratio = if total > 0 {
                    Some(available as f64 / total as f64)
                } else {
                    None
                };
                let value = if let Some(ratio) = ratio {
                    format!(
                        "{} ({})",
                        format_bytes(available * 1024),
                        format_percent(ratio)
                    )
                } else {
                    format_bytes(available * 1024)
                };
                view.add_kv("Memory Available", value);
            }
        }

        if let Some(swap) = body.get("swap_kb").and_then(Value::as_object) {
            if let Some(total) = swap.get("total").and_then(Value::as_u64) {
                view.add_kv("Swap Total", format_bytes(total * 1024));
            }
            if let Some(free) = swap.get("free").and_then(Value::as_u64) {
                view.add_kv("Swap Free", format_bytes(free * 1024));
            }
        }
    }

    fn populate_storage(view: &mut SectionView, body: &Value) {
        if let Some(mounts) = body.get("mounts").and_then(Value::as_array) {
            let mut entries: Vec<(f64, Vec<String>)> = mounts
                .iter()
                .filter_map(|mount| {
                    let mount_point = mount.get("mount_point")?.as_str()?.to_string();
                    let fs_type = mount.get("fs_type").and_then(Value::as_str).unwrap_or("-");
                    let used = mount
                        .get("used_bytes")
                        .and_then(Value::as_u64)
                        .map(format_bytes)
                        .unwrap_or_else(|| "-".to_string());
                    let total = mount
                        .get("total_bytes")
                        .and_then(Value::as_u64)
                        .map(format_bytes)
                        .unwrap_or_else(|| "-".to_string());
                    let ratio = mount
                        .get("usage_ratio")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let usage = format_percent(ratio);
                    Some((
                        ratio,
                        vec![mount_point, fs_type.to_string(), used, total, usage],
                    ))
                })
                .collect();

            entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
            let rows: Vec<Vec<String>> = entries.into_iter().map(|(_, row)| row).take(10).collect();

            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Top Mounts".to_string()),
                    headers: vec![
                        "Mount".to_string(),
                        "FS".to_string(),
                        "Used".to_string(),
                        "Total".to_string(),
                        "Usage".to_string(),
                    ],
                    rows,
                });
            }
        }

        if let Some(totals) = body.get("totals").and_then(Value::as_object) {
            if let Some(total) = totals.get("total_bytes").and_then(Value::as_u64) {
                view.add_kv("Total Capacity", format_bytes(total));
            }
            if let Some(used) = totals.get("used_bytes").and_then(Value::as_u64) {
                view.add_kv("Used Capacity", format_bytes(used));
            }
            if let Some(available) = totals.get("available_bytes").and_then(Value::as_u64) {
                view.add_kv("Available", format_bytes(available));
            }
        }
    }

    fn populate_services(view: &mut SectionView, body: &Value) {
        if let Some(running) = body.get("running").and_then(Value::as_array) {
            let mut rows = Vec::new();
            for entry in running.iter().take(12) {
                let unit = entry
                    .get("unit")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)");
                let description = entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let state = format_service_state(entry);
                rows.push(vec![unit.to_string(), description.to_string(), state]);
            }
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some(format!("Running Services ({} total)", running.len())),
                    headers: vec![
                        "Unit".to_string(),
                        "Description".to_string(),
                        "State".to_string(),
                    ],
                    rows,
                });
            }
        }

        if let Some(failed) = body.get("failed").and_then(Value::as_array) {
            let mut rows = Vec::new();
            for entry in failed.iter().take(12) {
                let unit = entry
                    .get("unit")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)");
                let description = entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let state = format_service_state(entry);
                rows.push(vec![unit.to_string(), description.to_string(), state]);
            }
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Failed Services".to_string()),
                    headers: vec![
                        "Unit".to_string(),
                        "Description".to_string(),
                        "State".to_string(),
                    ],
                    rows,
                });
            }
        }
    }

    fn format_service_state(value: &Value) -> String {
        let active = value.get("active").and_then(Value::as_str).unwrap_or("?");
        let sub = value.get("sub").and_then(Value::as_str).unwrap_or("?");
        format!("{active} / {sub}")
    }

    fn populate_network(view: &mut SectionView, body: &Value) {
        if let Some(interfaces) = body.get("interfaces").and_then(Value::as_array) {
            let mut rows = Vec::new();
            for iface in interfaces.iter().take(10) {
                let name = iface.get("name").and_then(Value::as_str).unwrap_or("?");
                let rx_bytes = iface
                    .get("rx_bytes")
                    .and_then(Value::as_u64)
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let tx_bytes = iface
                    .get("tx_bytes")
                    .and_then(Value::as_u64)
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let rx_packets = iface
                    .get("rx_packets")
                    .and_then(Value::as_u64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let tx_packets = iface
                    .get("tx_packets")
                    .and_then(Value::as_u64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                rows.push(vec![
                    name.to_string(),
                    rx_bytes,
                    tx_bytes,
                    rx_packets,
                    tx_packets,
                ]);
            }
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Network Interfaces".to_string()),
                    headers: vec![
                        "Interface".to_string(),
                        "RX".to_string(),
                        "TX".to_string(),
                        "RX packets".to_string(),
                        "TX packets".to_string(),
                    ],
                    rows,
                });
            }
        }

        if let Some(listeners) = body.get("listeners").and_then(Value::as_object) {
            if let Some(counts) = listeners.get("counts").and_then(Value::as_object) {
                let mut rows = Vec::new();
                for (proto, value) in counts.iter() {
                    if let Some(count) = value.as_u64() {
                        rows.push(vec![proto.to_string(), count.to_string()]);
                    }
                }
                if !rows.is_empty() {
                    view.add_table(TableView {
                        title: Some("Listening sockets".to_string()),
                        headers: vec!["Protocol".to_string(), "Count".to_string()],
                        rows,
                    });
                }
            }

            if let Some(samples) = listeners.get("samples").and_then(Value::as_array) {
                let items: Vec<String> = samples
                    .iter()
                    .take(10)
                    .map(|sample| {
                        let addr = sample
                            .get("local_address")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        let proto = sample
                            .get("protocol")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        let state = sample
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("listening");
                        format!("{proto} {addr} ({state})")
                    })
                    .collect();
                if !items.is_empty() {
                    view.add_list(ListView {
                        title: Some("Sample listeners".to_string()),
                        items,
                    });
                }
            }
        }
    }

    fn populate_journal(view: &mut SectionView, body: &Value) {
        if let Some(entries) = body.get("entries").and_then(Value::as_array) {
            let items: Vec<String> = entries
                .iter()
                .take(20)
                .map(|entry| {
                    let timestamp = entry
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let source = entry.get("source").and_then(Value::as_str).unwrap_or("?");
                    let message = entry
                        .get("message")
                        .and_then(Value::as_str)
                        .map(truncate)
                        .unwrap_or_else(|| "(no message)".to_string());
                    format!("{timestamp} — {source}: {message}")
                })
                .collect();
            if !items.is_empty() {
                view.add_list(ListView {
                    title: Some("Recent journal entries".to_string()),
                    items,
                });
            }
        }
    }

    fn populate_cron(view: &mut SectionView, body: &Value) {
        if let Some(system) = body.get("system_crontab").and_then(Value::as_array) {
            let rows: Vec<Vec<String>> = system
                .iter()
                .map(|entry| {
                    vec![
                        entry
                            .get("schedule")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        entry
                            .get("user")
                            .and_then(Value::as_str)
                            .unwrap_or("root")
                            .to_string(),
                        entry
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                    ]
                })
                .collect();
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("System crontab".to_string()),
                    headers: vec![
                        "Schedule".to_string(),
                        "User".to_string(),
                        "Command".to_string(),
                    ],
                    rows,
                });
            }
        }

        if let Some(files) = body.get("cron_d").and_then(Value::as_array) {
            for file in files.iter() {
                let path = file
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("/etc/cron.d");
                if let Some(entries) = file.get("entries").and_then(Value::as_array) {
                    let rows: Vec<Vec<String>> = entries
                        .iter()
                        .map(|entry| {
                            vec![
                                entry
                                    .get("schedule")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string(),
                                entry
                                    .get("user")
                                    .and_then(Value::as_str)
                                    .unwrap_or("root")
                                    .to_string(),
                                entry
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string(),
                            ]
                        })
                        .collect();
                    if !rows.is_empty() {
                        view.add_table(TableView {
                            title: Some(path.to_string()),
                            headers: vec![
                                "Schedule".to_string(),
                                "User".to_string(),
                                "Command".to_string(),
                            ],
                            rows,
                        });
                    }
                }
            }
        }
    }

    fn populate_docker(view: &mut SectionView, body: &Value) {
        if let Some(engine) = body.get("engine").and_then(Value::as_object) {
            if let Some(status) = engine.get("status").and_then(Value::as_str) {
                view.add_kv("Engine status", status);
            }
            if let Some(version) = engine.get("version").and_then(Value::as_str) {
                view.add_kv("Engine version", version);
            }
            if let Some(api) = engine.get("api_version").and_then(Value::as_str) {
                view.add_kv("API version", api);
            }
        }

        if let Some(containers) = body.get("containers").and_then(Value::as_array) {
            let rows: Vec<Vec<String>> = containers
                .iter()
                .take(12)
                .map(|container| {
                    let name = container
                        .get("names")
                        .and_then(Value::as_array)
                        .and_then(|arr| arr.iter().filter_map(Value::as_str).next())
                        .or_else(|| container.get("id").and_then(Value::as_str))
                        .unwrap_or("unknown");
                    let image = container
                        .get("image")
                        .and_then(Value::as_str)
                        .unwrap_or("-");
                    let state = container
                        .get("state")
                        .and_then(Value::as_str)
                        .or_else(|| container.get("status").and_then(Value::as_str))
                        .unwrap_or("?");
                    vec![name.to_string(), image.to_string(), state.to_string()]
                })
                .collect();
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Containers".to_string()),
                    headers: vec!["Name".to_string(), "Image".to_string(), "State".to_string()],
                    rows,
                });
            }
        }
    }

    fn populate_containers(view: &mut SectionView, body: &Value) {
        if let Some(runtimes) = body.get("runtimes").and_then(Value::as_array) {
            let items: Vec<String> = runtimes
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
            if !items.is_empty() {
                view.add_list(ListView {
                    title: Some("Detected runtimes".to_string()),
                    items,
                });
            }
        }
    }

    fn populate_users(view: &mut SectionView, body: &Value) {
        if let Some(users) = body.get("users").and_then(Value::as_array) {
            let total = users.len();
            let system = users
                .iter()
                .filter(|user| user.get("system").and_then(Value::as_bool).unwrap_or(false))
                .count();
            let regular = total.saturating_sub(system);
            view.add_kv("Users", format!("{} total", total));
            view.add_kv("System users", system.to_string());
            view.add_kv("Regular users", regular.to_string());

            let rows: Vec<Vec<String>> = users
                .iter()
                .take(12)
                .map(|user| {
                    let name = user.get("name").and_then(Value::as_str).unwrap_or("?");
                    let uid = user
                        .get("uid")
                        .and_then(Value::as_u64)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let shell = user.get("shell").and_then(Value::as_str).unwrap_or("-");
                    let role = if user.get("system").and_then(Value::as_bool).unwrap_or(false) {
                        "system"
                    } else {
                        "regular"
                    };
                    vec![name.to_string(), uid, shell.to_string(), role.to_string()]
                })
                .collect();
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Sample accounts".to_string()),
                    headers: vec![
                        "User".to_string(),
                        "UID".to_string(),
                        "Shell".to_string(),
                        "Type".to_string(),
                    ],
                    rows,
                });
            }
        }
    }

    fn populate_generic(view: &mut SectionView, body: &Value) {
        match body {
            Value::Object(map) => {
                for (key, value) in map.iter() {
                    view.add_kv(key, summarize_value(value));
                }
            }
            Value::Array(items) => {
                let list: Vec<String> = items.iter().take(20).map(summarize_value).collect();
                if !list.is_empty() {
                    view.add_list(ListView {
                        title: None,
                        items: list,
                    });
                }
            }
            Value::String(s) => {
                view.paragraph = Some(s.clone());
            }
            Value::Number(num) => {
                view.paragraph = Some(num.to_string());
            }
            Value::Bool(b) => {
                view.paragraph = Some(b.to_string());
            }
            Value::Null => {}
        }
    }

    fn status_class(status: &SectionStatus) -> &'static str {
        match status {
            SectionStatus::Success => "success",
            SectionStatus::Degraded => "degraded",
            SectionStatus::Error => "error",
        }
    }

    fn status_label(status: &SectionStatus) -> String {
        let mut label = status.to_string();
        if let Some(first) = label.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        label
    }

    fn format_bytes(bytes: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{} {}", bytes, UNITS[unit])
        } else {
            format!("{:.1} {}", value, UNITS[unit])
        }
    }

    fn format_percent(ratio: f64) -> String {
        format!("{:.1}%", ratio * 100.0)
    }

    fn summarize_value(value: &Value) -> String {
        match value {
            Value::Null => "n/a".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(num) => num.to_string(),
            Value::String(text) => truncate(text),
            Value::Array(arr) => format!("{} entries", arr.len()),
            Value::Object(map) => format!("{} keys", map.len()),
        }
    }

    fn truncate(input: &str) -> String {
        if input.len() > 120 {
            format!("{}…", &input[..117])
        } else {
            input.to_string()
        }
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
