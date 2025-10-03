use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
                let ssh_summary = summarize_ssh_activity(&entries);
                let body = json!({
                    "source": "journalctl --output=json",
                    "entries": entries,
                    "ssh_summary": ssh_summary,
                });

                let mut section = Section::success("journal", "systemd journal", body);
                if let Some(summary) = section.body.get("ssh_summary").and_then(Value::as_object) {
                    let invalid = summary
                        .get("invalid_user_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let failures = summary
                        .get("auth_failure_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    section.summary = Some(format!(
                        "Captured {} entries (SSH invalid users: {}, auth failures: {})",
                        entries.len(),
                        invalid,
                        failures
                    ));
                } else {
                    section.summary = Some(format!("Captured {} entries", entries.len()));
                }
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

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct SshSummary {
    invalid_user_count: u64,
    auth_failure_count: u64,
    top_usernames: Vec<CountEntry>,
    top_hosts: Vec<CountEntry>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct CountEntry {
    name: String,
    count: u64,
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

fn summarize_ssh_activity(entries: &[JournalEntry]) -> Option<SshSummary> {
    let mut invalid_user = 0u64;
    let mut auth_failures = 0u64;
    let mut usernames: HashMap<String, u64> = HashMap::new();
    let mut hosts: HashMap<String, u64> = HashMap::new();

    for entry in entries {
        let source = entry.source.as_deref().unwrap_or("").to_lowercase();
        if !source.contains("ssh") {
            continue;
        }

        let message_lower = entry.message.to_lowercase();
        if message_lower.contains("invalid user") {
            invalid_user += 1;
            if let Some(username) = extract_after(&message_lower, "invalid user") {
                *usernames.entry(username).or_insert(0) += 1;
            }
        }

        if message_lower.contains("failed password")
            || message_lower.contains("authentication failure")
        {
            auth_failures += 1;
            if let Some(username) = extract_username_from_failure(&message_lower) {
                *usernames.entry(username).or_insert(0) += 1;
            }
        }

        if let Some(host) = extract_after(&message_lower, "from") {
            *hosts.entry(host).or_insert(0) += 1;
        }
    }

    if invalid_user == 0 && auth_failures == 0 {
        return None;
    }

    let top_usernames = top_counts(usernames);
    let top_hosts = top_counts(hosts);

    Some(SshSummary {
        invalid_user_count: invalid_user,
        auth_failure_count: auth_failures,
        top_usernames,
        top_hosts,
    })
}

fn top_counts(map: HashMap<String, u64>) -> Vec<CountEntry> {
    let mut counts: Vec<_> = map.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts
        .into_iter()
        .take(5)
        .map(|(name, count)| CountEntry { name, count })
        .collect()
}

fn extract_after(message: &str, marker: &str) -> Option<String> {
    message
        .split(marker)
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .map(|token| {
            token
                .trim_matches(|c: char| !matches!(c, 'a'..='z' | '0'..='9' | '.' | ':' | '-'))
                .to_string()
        })
        .filter(|token| !token.is_empty())
}

fn extract_username_from_failure(message: &str) -> Option<String> {
    if let Some(segment) = message.split("for").nth(1) {
        return segment
            .split_whitespace()
            .next()
            .map(|token| {
                token
                    .trim_matches(|c: char| !matches!(c, 'a'..='z' | '0'..='9' | '-' | '_' | '.' ))
                    .to_string()
            })
            .filter(|token| !token.is_empty());
    }
    None
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
