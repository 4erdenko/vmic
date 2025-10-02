use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
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
        let sudo = self.users.iter().filter(|user| user.sudo).count();
        let interactive = self.users.iter().filter(|user| user.interactive).count();
        format!(
            "{} users ({} system, {} interactive, {} sudo)",
            total, system, interactive, sudo
        )
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
    interactive: bool,
    sudo: bool,
}

fn build_snapshot() -> Result<UsersSnapshot> {
    let mut users = read_passwd(Path::new("/etc/passwd"))?;
    let groups = read_groups(Path::new("/etc/group")).unwrap_or_default();
    let privileged_groups = ["sudo", "wheel", "admin"];

    let mut privileged_members: HashSet<String> = HashSet::new();
    let mut privileged_gids: HashSet<u32> = HashSet::new();

    for group in &groups {
        if privileged_groups.contains(&group.name.as_str()) {
            privileged_gids.insert(group.gid);
            for member in &group.members {
                privileged_members.insert(member.clone());
            }
        }
    }

    for user in users.iter_mut() {
        if privileged_gids.contains(&user.gid) {
            privileged_members.insert(user.name.clone());
        }
        user.sudo = privileged_members.contains(&user.name);
    }

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
        interactive: is_interactive_shell(parts[6]),
        sudo: false,
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

#[derive(Debug)]
struct GroupEntry {
    name: String,
    gid: u32,
    members: Vec<String>,
}

fn read_groups(path: &Path) -> Result<Vec<GroupEntry>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_groups(&content))
}

fn parse_groups(content: &str) -> Vec<GroupEntry> {
    content
        .lines()
        .filter_map(|line| parse_group_line(line).ok())
        .collect()
}

fn parse_group_line(line: &str) -> Result<GroupEntry> {
    if line.trim().is_empty() || line.starts_with('#') {
        anyhow::bail!("ignored line");
    }

    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() < 4 {
        anyhow::bail!("invalid group entry");
    }

    let gid: u32 = parts[2]
        .parse()
        .with_context(|| format!("invalid gid for group {}", parts[0]))?;
    let members = parts[3]
        .split(',')
        .filter(|member| !member.is_empty())
        .map(|member| member.to_string())
        .collect();

    Ok(GroupEntry {
        name: parts[0].to_string(),
        gid,
        members,
    })
}

fn is_interactive_shell(shell: &str) -> bool {
    matches!(
        shell,
        "/bin/sh"
            | "/bin/bash"
            | "/usr/bin/bash"
            | "/bin/zsh"
            | "/usr/bin/zsh"
            | "/bin/fish"
            | "/usr/bin/fish"
            | "/usr/bin/tmux"
            | "/bin/tcsh"
            | "/bin/csh"
    )
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
        assert!(user.interactive);
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
                    interactive: true,
                    sudo: true,
                },
                UserRecord {
                    name: "alice".into(),
                    uid: 1000,
                    gid: 1000,
                    home: "/home/alice".into(),
                    shell: "/bin/bash".into(),
                    system: false,
                    interactive: true,
                    sudo: false,
                },
            ],
        };

        assert_eq!(
            snapshot.summary(),
            "2 users (1 system, 2 interactive, 1 sudo)"
        );
    }

    #[test]
    fn parse_group_line_extracts_members() {
        let line = "sudo:x:27:alice,bob";
        let group = parse_group_line(line).expect("group");
        assert_eq!(group.name, "sudo");
        assert_eq!(group.gid, 27);
        assert_eq!(group.members.len(), 2);
    }
}
