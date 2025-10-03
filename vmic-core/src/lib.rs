use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use vmic_sdk::{self, CollectionContext, Section};

use crate::health::{HealthDigest, build_health_digest};
pub use health::{DigestThresholds, Severity};

pub use vmic_sdk::{CollectionContext as Context, SectionStatus};

pub mod schema;

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
        let start = Instant::now();
        let result = collector.collect(ctx);
        let elapsed_ms = start.elapsed().as_millis() as u64;

        let mut section = match result {
            Ok(section) => section,
            Err(error) => Section::error(metadata.id, metadata.title, error.to_string()),
        };
        section.duration_ms = Some(elapsed_ms);
        sections.push(section);
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
            .get("operating_mounts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for mount in mounts {
            let Some(point) = mount.get("mount_point").and_then(Value::as_str) else {
                continue;
            };
            let operational = mount
                .get("operational")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !operational {
                continue;
            }

            let Some(ratio) = mount.get("usage_ratio").and_then(Value::as_f64) else {
                continue;
            };

            let read_only = mount
                .get("read_only")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if read_only {
                continue;
            }

            let fs_type = mount.get("fs_type").and_then(Value::as_str).unwrap_or("");

            let available_bytes = mount
                .get("available_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let free_gib = available_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

            let inodes_ratio = mount
                .get("inodes_usage_ratio")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);

            let mut severity = Severity::Info;
            let mut reasons: Vec<String> = Vec::new();

            fn escalate(current: &mut Severity, new: Severity) {
                if new > *current {
                    *current = new;
                }
            }

            if ratio >= thresholds.disk_critical {
                escalate(&mut severity, Severity::Critical);
                reasons.push(format!("usage {:.1}%", ratio * 100.0));
            } else if ratio >= thresholds.disk_warning {
                escalate(&mut severity, Severity::Warning);
                reasons.push(format!("usage {:.1}%", ratio * 100.0));
            }

            if free_gib <= 2.0 {
                escalate(&mut severity, Severity::Critical);
                reasons.push(format!("free space {:.2} GiB", free_gib));
            } else if free_gib <= 5.0 {
                escalate(&mut severity, Severity::Warning);
                reasons.push(format!("free space {:.2} GiB", free_gib));
            }

            if inodes_ratio >= 0.90 {
                escalate(&mut severity, Severity::Critical);
                reasons.push(format!("inode usage {:.1}%", inodes_ratio * 100.0));
            } else if inodes_ratio >= 0.80 {
                escalate(&mut severity, Severity::Warning);
                reasons.push(format!("inode usage {:.1}%", inodes_ratio * 100.0));
            }

            if matches!(point, "/boot" | "/boot/efi") {
                if free_gib <= 0.25 {
                    escalate(&mut severity, Severity::Critical);
                    reasons.push("boot volume nearly full".to_string());
                } else if free_gib <= 0.5 {
                    escalate(&mut severity, Severity::Warning);
                    reasons.push("boot volume low free space".to_string());
                }
            }

            if severity == Severity::Info {
                continue;
            }

            let mut message = format!("Mount {} ({}): {:.1}% used", point, fs_type, ratio * 100.0);
            if !reasons.is_empty() {
                message.push_str(" — ");
                message.push_str(&reasons.join(", "));
            }

            findings.push(CriticalFinding::new(section, severity, message));
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

        let Some(memory) = section.body.get("memory").and_then(Value::as_object) else {
            return;
        };

        if let Some(host) = memory.get("host").and_then(Value::as_object) {
            let total = host.get("total_bytes").and_then(Value::as_u64).unwrap_or(0);
            let available = host
                .get("available_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);

            if total > 0 {
                let ratio = available as f64 / total as f64;
                let severity = if ratio <= thresholds.memory_critical {
                    Some(Severity::Critical)
                } else if ratio <= thresholds.memory_warning {
                    Some(Severity::Warning)
                } else {
                    None
                };

                if let Some(severity) = severity {
                    let available_gib = available as f64 / (1024.0 * 1024.0 * 1024.0);
                    let message = format!(
                        "Host memory {:.1}% available ({:.2} GiB free)",
                        ratio * 100.0,
                        available_gib
                    );
                    findings.push(CriticalFinding::new(section, severity, message));
                }
            }
        }

        if let Some(cgroup) = memory.get("cgroup").and_then(Value::as_object) {
            let limit = cgroup
                .get("limit_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let usage = cgroup
                .get("usage_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);

            if limit > 0 {
                let remaining_ratio = if usage >= limit {
                    0.0
                } else {
                    (limit - usage) as f64 / limit as f64
                };

                let severity = if remaining_ratio <= thresholds.memory_critical {
                    Some(Severity::Critical)
                } else if remaining_ratio <= thresholds.memory_warning {
                    Some(Severity::Warning)
                } else {
                    None
                };

                if let Some(severity) = severity {
                    let remaining_gib = if usage >= limit {
                        0.0
                    } else {
                        (limit - usage) as f64 / (1024.0 * 1024.0 * 1024.0)
                    };
                    let message = format!(
                        "Cgroup memory {:.1}% headroom ({:.2} GiB free of limit)",
                        remaining_ratio * 100.0,
                        remaining_gib
                    );
                    findings.push(CriticalFinding::new(section, severity, message));
                }
            }
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
        id: String,
        title: String,
        status_class: &'static str,
        status_label: String,
        summary: Option<String>,
        notes: Vec<String>,
        key_values: Vec<KeyValue>,
        tables: Vec<TableView>,
        lists: Vec<ListView>,
        paragraph: Option<String>,
        duration_label: String,
        has_key_values: bool,
        has_tables: bool,
        has_lists: bool,
        has_notes: bool,
        has_duration: bool,
    }

    impl SectionView {
        fn new(section: &super::Section) -> Self {
            let duration_label = format_duration(section.duration_ms).unwrap_or_default();
            Self {
                id: section.id.to_string(),
                title: section.title.to_string(),
                status_class: status_class(&section.status),
                status_label: status_label(&section.status),
                summary: section.summary.clone(),
                notes: section.notes.clone(),
                key_values: Vec::new(),
                tables: Vec::new(),
                lists: Vec::new(),
                paragraph: None,
                duration_label,
                has_key_values: false,
                has_tables: false,
                has_lists: false,
                has_notes: !section.notes.is_empty(),
                has_duration: section.duration_ms.is_some(),
            }
        }

        fn add_kv<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
            self.key_values.push(KeyValue {
                key: key.into(),
                value: value.into(),
            });
        }

        fn add_table(&mut self, mut table: TableView) {
            if !table.rows.is_empty() {
                table.ensure_row_classes();
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
            self.has_notes = !self.notes.is_empty();
            self.has_duration = !self.duration_label.is_empty();
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
        row_classes: Vec<String>,
    }

    impl TableView {
        fn ensure_row_classes(&mut self) {
            if self.row_classes.len() < self.rows.len() {
                self.row_classes.resize(self.rows.len(), String::new());
            }
        }
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

        if let Some(memory) = body.get("memory").and_then(Value::as_object) {
            if let Some(host) = memory.get("host").and_then(Value::as_object) {
                if let Some(total) = host.get("total_bytes").and_then(Value::as_u64) {
                    view.add_kv("Host Memory Total", format_bytes(total));
                }
                if let Some(available) = host.get("available_bytes").and_then(Value::as_u64) {
                    let mut value = format_bytes(available);
                    if let Some(ratio) = host.get("usage_ratio").and_then(Value::as_f64) {
                        value = format!(
                            "{} free ({:.1}% used)",
                            format_bytes(available),
                            ratio * 100.0
                        );
                    }
                    view.add_kv("Host Memory", value);
                }
            }

            if let Some(cgroup) = memory.get("cgroup").and_then(Value::as_object) {
                if let Some(limit) = cgroup.get("limit_bytes").and_then(Value::as_u64) {
                    view.add_kv("Cgroup Limit", format_bytes(limit));
                }
                if let (Some(usage), Some(limit)) = (
                    cgroup.get("usage_bytes").and_then(Value::as_u64),
                    cgroup.get("limit_bytes").and_then(Value::as_u64),
                ) {
                    let remaining = limit.saturating_sub(usage);
                    let ratio = if limit > 0 {
                        format_percent(remaining as f64 / limit as f64)
                    } else {
                        "n/a".to_string()
                    };
                    view.add_kv(
                        "Cgroup Remaining",
                        format!("{} ({})", format_bytes(remaining), ratio),
                    );
                }
            }

            if let Some(swap) = memory.get("swap").and_then(Value::as_object) {
                if let Some(total) = swap.get("total_bytes").and_then(Value::as_u64) {
                    view.add_kv("Swap Total", format_bytes(total));
                }
                if let Some(free) = swap.get("free_bytes").and_then(Value::as_u64) {
                    view.add_kv("Swap Free", format_bytes(free));
                }

                if let Some(devices) = swap.get("devices").and_then(Value::as_array) {
                    if !devices.is_empty() {
                        let rows: Vec<Vec<String>> = devices
                            .iter()
                            .take(6)
                            .map(|device| {
                                vec![
                                    device
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("-")
                                        .to_string(),
                                    device
                                        .get("kind")
                                        .and_then(Value::as_str)
                                        .unwrap_or("-")
                                        .to_string(),
                                    device
                                        .get("priority")
                                        .and_then(Value::as_i64)
                                        .map(|p| p.to_string())
                                        .unwrap_or_else(|| "0".to_string()),
                                    device
                                        .get("used_bytes")
                                        .and_then(Value::as_u64)
                                        .map(format_bytes)
                                        .unwrap_or_else(|| "-".to_string()),
                                    device
                                        .get("size_bytes")
                                        .and_then(Value::as_u64)
                                        .map(format_bytes)
                                        .unwrap_or_else(|| "-".to_string()),
                                ]
                            })
                            .collect();

                        view.add_table(TableView {
                            title: Some("Swap Devices".to_string()),
                            headers: vec![
                                "Device".to_string(),
                                "Type".to_string(),
                                "Priority".to_string(),
                                "Used".to_string(),
                                "Size".to_string(),
                            ],
                            rows,
                            row_classes: Vec::new(),
                        });
                    }
                }

                if let Some(zram) = swap.get("zram_devices").and_then(Value::as_array) {
                    if !zram.is_empty() {
                        let rows: Vec<Vec<String>> = zram
                            .iter()
                            .take(6)
                            .map(|device| {
                                vec![
                                    device
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("-")
                                        .to_string(),
                                    device
                                        .get("disksize_bytes")
                                        .and_then(Value::as_u64)
                                        .map(format_bytes)
                                        .unwrap_or_else(|| "-".to_string()),
                                    device
                                        .get("compressed_bytes")
                                        .and_then(Value::as_u64)
                                        .map(format_bytes)
                                        .unwrap_or_else(|| "-".to_string()),
                                    device
                                        .get("active")
                                        .and_then(Value::as_bool)
                                        .map(|flag| if flag { "yes" } else { "no" }.to_string())
                                        .unwrap_or_else(|| "no".to_string()),
                                ]
                            })
                            .collect();

                        view.add_table(TableView {
                            title: Some("ZRAM Devices".to_string()),
                            headers: vec![
                                "Device".to_string(),
                                "Configured".to_string(),
                                "Compressed".to_string(),
                                "Active".to_string(),
                            ],
                            rows,
                            row_classes: Vec::new(),
                        });
                    }
                }
            }
        }

        if let Some(psi) = body.get("psi").and_then(Value::as_object) {
            let mut rows = Vec::new();
            if let Some(cpu) = psi.get("cpu").and_then(Value::as_object) {
                if let Some(metrics) = cpu.get("some").and_then(Value::as_object) {
                    rows.push(vec![
                        "CPU (some)".to_string(),
                        metrics
                            .get("avg10")
                            .and_then(Value::as_f64)
                            .map(|v| format!("{:.2}", v))
                            .unwrap_or_else(|| "-".to_string()),
                        metrics
                            .get("avg60")
                            .and_then(Value::as_f64)
                            .map(|v| format!("{:.2}", v))
                            .unwrap_or_else(|| "-".to_string()),
                        metrics
                            .get("avg300")
                            .and_then(Value::as_f64)
                            .map(|v| format!("{:.2}", v))
                            .unwrap_or_else(|| "-".to_string()),
                    ]);
                }
            }
            for key in ["memory", "io"] {
                if let Some(resource) = psi.get(key).and_then(Value::as_object) {
                    if let Some(metrics) = resource.get("some").and_then(Value::as_object) {
                        rows.push(vec![
                            format!("{} (some)", key),
                            metrics
                                .get("avg10")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                            metrics
                                .get("avg60")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                            metrics
                                .get("avg300")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                        ]);
                    }
                    if let Some(metrics) = resource.get("full").and_then(Value::as_object) {
                        rows.push(vec![
                            format!("{} (full)", key),
                            metrics
                                .get("avg10")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                            metrics
                                .get("avg60")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                            metrics
                                .get("avg300")
                                .and_then(Value::as_f64)
                                .map(|v| format!("{:.2}", v))
                                .unwrap_or_else(|| "-".to_string()),
                        ]);
                    }
                }
            }

            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Pressure Stall (avg%)".to_string()),
                    headers: vec![
                        "Resource".to_string(),
                        "avg10".to_string(),
                        "avg60".to_string(),
                        "avg300".to_string(),
                    ],
                    rows,
                    row_classes: Vec::new(),
                });
            }
        }
    }

    fn populate_storage(view: &mut SectionView, body: &Value) {
        if let Some(mounts) = body.get("operating_mounts").and_then(Value::as_array) {
            let mut entries: Vec<(f64, Vec<String>)> = mounts
                .iter()
                .filter_map(|mount| {
                    let mount_point = mount.get("mount_point")?.as_str()?.to_string();
                    let fs_type = mount.get("fs_type").and_then(Value::as_str).unwrap_or("-");
                    let read_only = if mount
                        .get("read_only")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        "ro"
                    } else {
                        "rw"
                    };
                    let used = mount
                        .get("used_bytes")
                        .and_then(Value::as_u64)
                        .map(format_bytes)
                        .unwrap_or_else(|| "-".to_string());
                    let free = mount
                        .get("available_bytes")
                        .and_then(Value::as_u64)
                        .map(format_bytes)
                        .unwrap_or_else(|| "-".to_string());
                    let ratio = mount
                        .get("usage_ratio")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let usage = format_percent(ratio);
                    let inode_ratio = mount
                        .get("inodes_usage_ratio")
                        .and_then(Value::as_f64)
                        .map(|ratio| format_percent(ratio))
                        .unwrap_or_else(|| "n/a".to_string());

                    Some((
                        ratio,
                        vec![
                            mount_point,
                            fs_type.to_string(),
                            read_only.to_string(),
                            used,
                            free,
                            usage,
                            inode_ratio,
                        ],
                    ))
                })
                .collect();

            entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
            let mut row_classes: Vec<String> = Vec::new();
            let rows: Vec<Vec<String>> = entries
                .into_iter()
                .map(|(ratio, row)| {
                    let class = if ratio >= 0.90 {
                        "row-critical"
                    } else if ratio >= 0.80 {
                        "row-warning"
                    } else {
                        ""
                    };
                    row_classes.push(class.to_string());
                    row
                })
                .take(12)
                .collect();

            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Operating Mounts".to_string()),
                    headers: vec![
                        "Mount".to_string(),
                        "FS".to_string(),
                        "Mode".to_string(),
                        "Used".to_string(),
                        "Free".to_string(),
                        "Usage".to_string(),
                        "Inodes".to_string(),
                    ],
                    rows,
                    row_classes,
                });
            }
        }

        if let Some(mounts) = body.get("pseudo_mounts").and_then(Value::as_array) {
            let mut rows = Vec::new();
            for mount in mounts.iter().take(12) {
                let mount_point = mount
                    .get("mount_point")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let fs_type = mount.get("fs_type").and_then(Value::as_str).unwrap_or("-");
                let usage = mount
                    .get("usage_ratio")
                    .and_then(Value::as_f64)
                    .map(format_percent)
                    .unwrap_or_else(|| "n/a".to_string());
                rows.push(vec![mount_point.to_string(), fs_type.to_string(), usage]);
            }
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Images & Pseudo FS".to_string()),
                    headers: vec!["Mount".to_string(), "FS".to_string(), "Usage".to_string()],
                    rows,
                    row_classes: Vec::new(),
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

        if let Some(docker) = body.get("docker").and_then(Value::as_object) {
            let root = docker
                .get("data_root")
                .and_then(Value::as_str)
                .unwrap_or("/var/lib/docker");
            view.add_kv("Docker data root", root.to_string());
            if let Some(total) = docker.get("total_bytes").and_then(Value::as_u64) {
                view.add_kv("Docker total", format_bytes(total));
            }
            if let Some(diff) = docker.get("overlay_bytes").and_then(Value::as_u64) {
                view.add_kv("Overlay diff", format_bytes(diff));
            }
            if let Some(logs) = docker.get("container_logs_bytes").and_then(Value::as_u64) {
                view.add_kv("Container logs", format_bytes(logs));
            }
            if let Some(volumes) = docker.get("volumes_bytes").and_then(Value::as_u64) {
                view.add_kv("Volumes", format_bytes(volumes));
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
                    row_classes: Vec::new(),
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
                    row_classes: Vec::new(),
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
                    row_classes: Vec::new(),
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
                        row_classes: Vec::new(),
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
                        let proc_details: Vec<String> = sample
                            .get("processes")
                            .and_then(Value::as_array)
                            .map(|processes| {
                                processes
                                    .iter()
                                    .take(3)
                                    .map(|proc| {
                                        let pid =
                                            proc.get("pid").and_then(Value::as_i64).unwrap_or(-1);
                                        let command = proc
                                            .get("command")
                                            .and_then(Value::as_str)
                                            .unwrap_or("?");
                                        let uid =
                                            proc.get("uid").and_then(Value::as_u64).unwrap_or(0);
                                        let container = proc
                                            .get("container")
                                            .and_then(Value::as_str)
                                            .unwrap_or("");
                                        if container.is_empty() {
                                            format!("pid {pid} {command} (uid {uid})")
                                        } else {
                                            format!(
                                                "pid {pid} {command} (uid {uid}, cgroup {container})"
                                            )
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        if proc_details.is_empty() {
                            format!("{proto} {addr} ({state})")
                        } else {
                            format!("{proto} {addr} ({state}) — {}", proc_details.join(", "))
                        }
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
        if let Some(summary) = body.get("ssh_summary").and_then(Value::as_object) {
            let invalid = summary
                .get("invalid_user_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let failures = summary
                .get("auth_failure_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            view.add_kv("SSH invalid users", invalid.to_string());
            view.add_kv("SSH auth failures", failures.to_string());

            if let Some(hosts) = summary.get("top_hosts").and_then(Value::as_array) {
                if !hosts.is_empty() {
                    let items: Vec<String> = hosts
                        .iter()
                        .take(5)
                        .map(|entry| {
                            let name = entry.get("name").and_then(Value::as_str).unwrap_or("-");
                            let count = entry.get("count").and_then(Value::as_u64).unwrap_or(0);
                            format!("{name} ({count})")
                        })
                        .collect();
                    view.add_list(ListView {
                        title: Some("Top SSH source IPs".to_string()),
                        items,
                    });
                }
            }

            if let Some(users) = summary.get("top_usernames").and_then(Value::as_array) {
                if !users.is_empty() {
                    let items: Vec<String> = users
                        .iter()
                        .take(5)
                        .map(|entry| {
                            let name = entry.get("name").and_then(Value::as_str).unwrap_or("-");
                            let count = entry.get("count").and_then(Value::as_u64).unwrap_or(0);
                            format!("{name} ({count})")
                        })
                        .collect();
                    view.add_list(ListView {
                        title: Some("Top SSH usernames".to_string()),
                        items,
                    });
                }
            }
        }

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
                    row_classes: Vec::new(),
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
                            row_classes: Vec::new(),
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
            let mut row_classes = Vec::new();
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
                    let state_lower = state.to_ascii_lowercase();
                    let class = if state_lower.contains("unhealthy") {
                        "row-critical"
                    } else if state_lower.contains("restarting") || state_lower.contains("exited") {
                        "row-warning"
                    } else {
                        ""
                    };
                    row_classes.push(class.to_string());
                    vec![name.to_string(), image.to_string(), state.to_string()]
                })
                .collect();
            if !rows.is_empty() {
                view.add_table(TableView {
                    title: Some("Containers".to_string()),
                    headers: vec!["Name".to_string(), "Image".to_string(), "State".to_string()],
                    rows,
                    row_classes,
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
            let interactive = users
                .iter()
                .filter(|user| {
                    user.get("interactive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .count();
            let sudo = users
                .iter()
                .filter(|user| user.get("sudo").and_then(Value::as_bool).unwrap_or(false))
                .count();
            view.add_kv("Users", format!("{} total", total));
            view.add_kv("System users", system.to_string());
            view.add_kv("Regular users", regular.to_string());
            view.add_kv("Interactive shells", interactive.to_string());
            view.add_kv("Sudo access", sudo.to_string());

            let mut row_classes = Vec::new();
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
                    let is_system = user.get("system").and_then(Value::as_bool).unwrap_or(false);
                    let role = if is_system { "system" } else { "regular" };
                    let interactive = if user
                        .get("interactive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        "yes"
                    } else {
                        "no"
                    };
                    let has_sudo = user.get("sudo").and_then(Value::as_bool).unwrap_or(false);
                    let sudo = if has_sudo { "yes" } else { "no" };
                    if has_sudo && !is_system {
                        row_classes.push("row-warning".to_string());
                    } else {
                        row_classes.push(String::new());
                    }
                    vec![
                        name.to_string(),
                        uid,
                        shell.to_string(),
                        role.to_string(),
                        interactive.to_string(),
                        sudo.to_string(),
                    ]
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
                        "Interactive".to_string(),
                        "Sudo".to_string(),
                    ],
                    rows,
                    row_classes,
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

    fn format_duration(duration_ms: Option<u64>) -> Option<String> {
        duration_ms.map(|ms| {
            if ms >= 10_000 {
                format!("{:.1}s", ms as f64 / 1000.0)
            } else if ms >= 1000 {
                format!("{:.2}s", ms as f64 / 1000.0)
            } else {
                format!("{} ms", ms)
            }
        })
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
    use jsonschema::JSONSchema;
    use serde_json::{Value, json};
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
    fn default_digest_thresholds_match_updated_values() {
        let thresholds = DigestThresholds::default();
        assert_eq!(thresholds.disk_warning, 0.90);
        assert_eq!(thresholds.disk_critical, 0.95);
        assert_eq!(thresholds.memory_warning, 0.10);
        assert_eq!(thresholds.memory_critical, 0.05);
    }

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
    fn report_json_conforms_to_schema() {
        let mut section = Section::success(
            "demo",
            "Demo Section",
            json!({
                "value": 42,
            }),
        );
        section.summary = Some("Demo summary".to_string());

        let report = Report::with_digest_config(vec![section], DigestThresholds::default());
        let compiled = JSONSchema::compile(schema::report_schema()).expect("schema compilation");
        let document = report.to_json_value();

        if let Err(errors) = compiled.validate(&document) {
            let collected: Vec<String> = errors.map(|err| format!("{}", err)).collect();
            panic!(
                "report JSON did not match schema:\n{}",
                collected.join("\n")
            );
        }
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
        assert!(html.contains("<nav class=\"toc\""));
        assert!(html.contains("class=\"card digest status-"));
        assert!(html.contains("section-summary"));
        assert!(html.contains("Back to top"));
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
                "operating_mounts": [
                    {
                        "mount_point": "/data",
                        "fs_type": "ext4",
                        "read_only": false,
                        "category": "operating",
                        "operational": true,
                        "total_bytes": 100_000_000_000u64,
                        "used_bytes": 95_000_000_000u64,
                        "available_bytes": 5_000_000_000u64,
                        "usage_ratio": 0.95,
                        "inodes_usage_ratio": 0.5
                    }
                ],
                "pseudo_mounts": [],
                "totals": json!({}),
                "docker": Value::Null
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
                "operating_mounts": [
                    {
                        "mount_point": "/data",
                        "fs_type": "ext4",
                        "read_only": false,
                        "category": "operating",
                        "operational": true,
                        "total_bytes": 100_000_000_000u64,
                        "used_bytes": 85_000_000_000u64,
                        "available_bytes": 15_000_000_000u64,
                        "usage_ratio": 0.85,
                        "inodes_usage_ratio": 0.5
                    }
                ],
                "pseudo_mounts": [],
                "totals": json!({}),
                "docker": Value::Null
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
