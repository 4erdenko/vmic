use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

const JOURNAL_LINES: &str = "50";

struct JournalCollector;

impl Collector for JournalCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "journal",
            title: "systemd journal",
            description: "Recent events from journald",
        }
    }

    fn collect(&self, ctx: &CollectionContext) -> Result<Section> {
        match gather_entries(ctx) {
            Ok(entries) => {
                let body = json!({
                    "source": "journalctl --output=json",
                    "entries": entries,
                });

                let mut section = Section::success("journal", "systemd journal", body);
                section.summary = Some(format!("Captured {} entries", entries.len()));
                Ok(section)
            }
            Err(err) => Ok(Section::degraded(
                "journal",
                "systemd journal",
                err.to_string(),
                json!({
                    "source": "journalctl --output=json",
                    "entries": Vec::<serde_json::Value>::new(),
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(JournalCollector)
}

register_collector!(create_collector);

#[derive(Debug, Deserialize)]
struct RawJournalEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime_timestamp: Option<String>,
    #[serde(rename = "MESSAGE")]
    message: Option<String>,
    #[serde(rename = "_SYSTEMD_UNIT")]
    systemd_unit: Option<String>,
    #[serde(rename = "_COMM")]
    comm: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct JournalEntry {
    timestamp: String,
    source: Option<String>,
    message: String,
}

fn gather_entries(ctx: &CollectionContext) -> Result<Vec<JournalEntry>> {
    let mut command = Command::new("journalctl");
    command
        .arg("--output=json")
        .arg("--no-pager")
        .arg("-n")
        .arg(JOURNAL_LINES);

    if let Some(since) = ctx.since() {
        command.arg("--since").arg(since);
    }

    let output = command.output().context("failed to execute journalctl")?;

    if !output.status.success() {
        return Err(anyhow!(
            "journalctl exited with status {}",
            output.status.code().unwrap_or_default()
        ));
    }

    let stdout = String::from_utf8(output.stdout).context("journalctl returned invalid UTF-8")?;
    parse_journal_stream(&stdout)
}

fn parse_journal_stream(content: &str) -> Result<Vec<JournalEntry>> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_journal_line)
        .collect()
}

fn parse_journal_line(line: &str) -> Result<JournalEntry> {
    let raw: RawJournalEntry = serde_json::from_str(line)
        .with_context(|| format!("failed to parse journald line: {}", line))?;

    let message = raw
        .message
        .and_then(|m| if m.trim().is_empty() { None } else { Some(m) })
        .unwrap_or_else(|| "(no message)".to_string());

    let timestamp = raw
        .realtime_timestamp
        .and_then(|ts| format_timestamp(&ts))
        .unwrap_or_else(|| "unknown".to_string());

    let source = raw.systemd_unit.or(raw.comm);

    Ok(JournalEntry {
        timestamp,
        source,
        message,
    })
}

fn format_timestamp(value: &str) -> Option<String> {
    let micros: u64 = value.parse().ok()?;
    let secs = micros / 1_000_000;
    let micros_remainder = micros % 1_000_000;
    let nanos = (micros_remainder as u32) * 1_000;
    let system_time = UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?;
    let datetime: DateTime<Utc> = DateTime::<Utc>::from(system_time);
    Some(datetime.to_rfc3339_opts(SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_extracts_fields() {
        let sample = r#"{"__REALTIME_TIMESTAMP":"1700000000000000","MESSAGE":"Service started","_SYSTEMD_UNIT":"demo.service"}"#;
        let entry = parse_journal_line(sample).expect("parse");
        assert_eq!(entry.message, "Service started");
        assert_eq!(entry.source.as_deref(), Some("demo.service"));
        assert!(entry.timestamp.starts_with("2023-"));
    }

    #[test]
    fn parse_stream_skips_empty_lines() {
        let sample = "\n\n";
        let entries = parse_journal_stream(sample).expect("parse");
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_line_handles_missing_fields() {
        let sample = r#"{"MESSAGE":"","_COMM":"bash"}"#;
        let entry = parse_journal_line(sample).expect("parse");
        assert_eq!(entry.message, "(no message)");
        assert_eq!(entry.source.as_deref(), Some("bash"));
        assert_eq!(entry.timestamp, "unknown");
    }
}
