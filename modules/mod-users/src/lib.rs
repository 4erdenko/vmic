use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::Path;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct UsersCollector;

impl Collector for UsersCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "users",
            title: "Local Users",
            description: "Accounts defined in /etc/passwd",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        match build_snapshot() {
            Ok(snapshot) => Ok(section_from_snapshot(&snapshot)),
            Err(error) => Ok(Section::degraded(
                "users",
                "Local Users",
                error.to_string(),
                json!({
                    "users": Vec::<serde_json::Value>::new(),
                }),
            )),
        }
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(UsersCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsersSnapshot {
    users: Vec<UserRecord>,
}

impl UsersSnapshot {
    fn summary(&self) -> String {
        let total = self.users.len();
        let system = self.users.iter().filter(|user| user.system).count();
        format!("{} users ({} system)", total, system)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct UserRecord {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
    system: bool,
}

fn build_snapshot() -> Result<UsersSnapshot> {
    let users = read_passwd(Path::new("/etc/passwd"))?;
    Ok(UsersSnapshot { users })
}

fn read_passwd(path: &Path) -> Result<Vec<UserRecord>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_passwd(&content))
}

fn parse_passwd(content: &str) -> Vec<UserRecord> {
    content
        .lines()
        .filter_map(|line| parse_passwd_line(line).ok())
        .collect()
}

fn parse_passwd_line(line: &str) -> Result<UserRecord> {
    if line.trim().is_empty() || line.starts_with('#') {
        anyhow::bail!("ignored line");
    }

    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() < 7 {
        anyhow::bail!("invalid passwd entry");
    }

    let uid: u32 = parts[2]
        .parse()
        .with_context(|| format!("invalid uid for {}", parts[0]))?;
    let gid: u32 = parts[3]
        .parse()
        .with_context(|| format!("invalid gid for {}", parts[0]))?;

    Ok(UserRecord {
        name: parts[0].to_string(),
        uid,
        gid,
        home: parts[5].to_string(),
        shell: parts[6].to_string(),
        system: uid < 1000,
    })
}

fn section_from_snapshot(snapshot: &UsersSnapshot) -> Section {
    let body = json!({
        "users": snapshot.users,
    });
    let mut section = Section::success("users", "Local Users", body);
    section.summary = Some(snapshot.summary());
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_passwd_line_returns_user() {
        let line = "root:x:0:0:root:/root:/bin/bash";
        let user = parse_passwd_line(line).expect("parsed user");
        assert_eq!(user.name, "root");
        assert!(user.system);
        assert_eq!(user.shell, "/bin/bash");
    }

    #[test]
    fn snapshot_summary_counts_users() {
        let snapshot = UsersSnapshot {
            users: vec![
                UserRecord {
                    name: "root".into(),
                    uid: 0,
                    gid: 0,
                    home: "/root".into(),
                    shell: "/bin/bash".into(),
                    system: true,
                },
                UserRecord {
                    name: "alice".into(),
                    uid: 1000,
                    gid: 1000,
                    home: "/home/alice".into(),
                    shell: "/bin/bash".into(),
                    system: false,
                },
            ],
        };

        assert_eq!(snapshot.summary(), "2 users (1 system)");
    }
}
