use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct CronCollector;

impl Collector for CronCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "cron",
            title: "Scheduled Jobs",
            description: "System cron configuration",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match build_snapshot() {
            Ok(snapshot) => Ok(section_from_snapshot(&snapshot)),
            Err(error) => Ok(Section::degraded(
                "cron",
                "Scheduled Jobs",
                error.to_string(),
                json!({
                    "system_crontab": Vec::<serde_json::Value>::new(),
                    "cron_d": Vec::<serde_json::Value>::new(),
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(CronCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CronEntry {
    schedule: String,
    user: String,
    command: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CronFileSummary {
    path: PathBuf,
    entries: Vec<CronEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronSnapshot {
    system_entries: Vec<CronEntry>,
    cron_d: Vec<CronFileSummary>,
}

impl CronSnapshot {
    fn summary(&self) -> String {
        let total: usize = self.system_entries.len()
            + self
                .cron_d
                .iter()
                .map(|file| file.entries.len())
                .sum::<usize>();
        format!("{} cron entries", total)
    }
}

fn build_snapshot() -> Result<CronSnapshot> {
    let system_entries = read_crontab(Path::new("/etc/crontab"))?;
    let cron_d = read_cron_directory(Path::new("/etc/cron.d"))?;

    Ok(CronSnapshot {
        system_entries,
        cron_d,
    })
}

fn read_crontab(path: &Path) -> Result<Vec<CronEntry>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(parse_crontab(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn read_cron_directory(path: &Path) -> Result<Vec<CronFileSummary>> {
    match fs::read_dir(path) {
        Ok(entries) => {
            let mut result = Vec::new();
            for entry in entries {
                let entry =
                    entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let file_path = entry.path();
                let entries = read_crontab(&file_path)?;
                result.push(CronFileSummary {
                    path: file_path,
                    entries,
                });
            }
            Ok(result)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("failed to list {}", path.display())),
    }
}

fn parse_crontab(content: &str) -> Vec<CronEntry> {
    content
        .lines()
        .filter_map(|line| parse_cron_line(line).ok())
        .collect()
}

fn parse_cron_line(line: &str) -> Result<CronEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        anyhow::bail!("ignored line");
    }

    let mut parts = trimmed.split_whitespace();
    let first = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing schedule"))?;

    if first.starts_with('@') {
        let user = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing user"))?;
        let command = parts.collect::<Vec<_>>().join(" ");

        if command.is_empty() {
            anyhow::bail!("missing command");
        }

        return Ok(CronEntry {
            schedule: first.to_string(),
            user: user.to_string(),
            command,
        });
    }

    let minute = first;
    let hour = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing hour"))?;
    let day_of_month = parts.next().ok_or_else(|| anyhow::anyhow!("missing day"))?;
    let month = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing month"))?;
    let day_of_week = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing weekday"))?;
    let user = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing user"))?;
    let command = parts.collect::<Vec<_>>().join(" ");

    if command.is_empty() {
        anyhow::bail!("missing command");
    }

    Ok(CronEntry {
        schedule: format!(
            "{} {} {} {} {}",
            minute, hour, day_of_month, month, day_of_week
        ),
        user: user.to_string(),
        command,
    })
}

fn section_from_snapshot(snapshot: &CronSnapshot) -> Section {
    let body = json!({
        "system_crontab": snapshot.system_entries,
        "cron_d": snapshot.cron_d,
    });
    let mut section = Section::success("cron", "Scheduled Jobs", body);
    section.summary = Some(snapshot.summary());
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cron_line_extracts_command() {
        let line = "0 5 * * * root /usr/bin/run-backup";
        let entry = parse_cron_line(line).expect("parsed cron");
        assert_eq!(entry.user, "root");
        assert!(entry.command.contains("run-backup"));
        assert!(entry.schedule.starts_with("0 5"));
    }

    #[test]
    fn parse_cron_line_supports_macros() {
        let line = "@daily root /usr/local/bin/backup";
        let entry = parse_cron_line(line).expect("parsed macro cron");
        assert_eq!(entry.schedule, "@daily");
        assert_eq!(entry.user, "root");
        assert_eq!(entry.command, "/usr/local/bin/backup");
    }

    #[test]
    fn parse_cron_line_rejects_macro_without_command() {
        let line = "@reboot root";
        let error = parse_cron_line(line).expect_err("missing macro command");
        assert!(error.to_string().contains("missing command"));
    }

    #[test]
    fn snapshot_summary_counts_entries() {
        let snapshot = CronSnapshot {
            system_entries: vec![CronEntry {
                schedule: "0 0 * * *".into(),
                user: "root".into(),
                command: "/bin/true".into(),
            }],
            cron_d: vec![CronFileSummary {
                path: PathBuf::from("/etc/cron.d/test"),
                entries: vec![CronEntry {
                    schedule: "*/5 * * * *".into(),
                    user: "alice".into(),
                    command: "/bin/echo".into(),
                }],
            }],
        };

        assert_eq!(snapshot.summary(), "2 cron entries");
    }
}
