use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::Path;
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct SecurityCollector;

impl Collector for SecurityCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "security",
            title: "Security Posture",
            description: "Key host hardening checks",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let mut notes = Vec::new();

        let sshd = match analyze_sshd_config(Path::new("/etc/ssh/sshd_config")) {
            Ok(analysis) => analysis,
            Err(error) => {
                notes.push(format!("sshd_config check failed: {error}"));
                SshdConfigAnalysis::default()
            }
        };

        let sudoers = match analyze_sudoers(Path::new("/etc/sudoers")) {
            Ok(analysis) => analysis,
            Err(error) => {
                notes.push(format!("sudoers check failed: {error}"));
                SudoersAnalysis::default()
            }
        };

        let cgroups = analyze_cgroups();

        let findings = sshd.findings.len() + sudoers.findings.len() + cgroups.findings.len();

        let body = json!({
            "sshd": sshd,
            "sudoers": sudoers,
            "cgroups": cgroups,
        });

        let mut section = if findings == 0 {
            let mut section = Section::success("security", "Security Posture", body);
            section.summary = Some("No high-risk findings detected".to_string());
            section
        } else {
            Section::degraded(
                "security",
                "Security Posture",
                format!("{} potential security issues", findings),
                body,
            )
        };

        section.notes = notes;
        Ok(section)
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(SecurityCollector)
}

register_collector!(create_collector);

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
struct Finding {
    message: String,
    severity: Severity,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Severity {
    #[default]
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
struct SshdConfigAnalysis {
    hardening_present: bool,
    findings: Vec<Finding>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
struct SudoersAnalysis {
    includes_dir: bool,
    findings: Vec<Finding>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
struct CgroupAnalysis {
    unified_hierarchy: bool,
    controllers: Vec<String>,
    findings: Vec<Finding>,
}

fn analyze_sshd_config(path: &Path) -> Result<SshdConfigAnalysis> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(analyze_sshd_config_from_str(&contents))
}

fn analyze_sshd_config_from_str(contents: &str) -> SshdConfigAnalysis {
    let mut analysis = SshdConfigAnalysis {
        hardening_present: false,
        findings: Vec::new(),
    };

    let mut password_auth = None;
    let mut permit_root = None;
    let mut challenge_response = None;
    let mut protocol = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or_default().to_ascii_lowercase();
        let value = parts.collect::<Vec<_>>().join(" ").to_ascii_lowercase();
        match key.as_str() {
            "passwordauthentication" => password_auth = Some(value),
            "permitrootlogin" => permit_root = Some(value),
            "challengeresponseauthentication" => challenge_response = Some(value),
            "protocol" => protocol = Some(value),
            "kexalgorithms" | "ciphers" | "macs" => analysis.hardening_present = true,
            _ => {}
        }
    }

    if password_auth.as_deref() == Some("yes") {
        analysis.findings.push(Finding {
            message: "PasswordAuthentication is enabled".to_string(),
            severity: Severity::Warning,
        });
    }

    if permit_root
        .as_deref()
        .map(|value| value == "yes" || value == "without-password")
        .unwrap_or(false)
    {
        analysis.findings.push(Finding {
            message: "PermitRootLogin allows direct root access".to_string(),
            severity: Severity::Critical,
        });
    }

    if challenge_response.as_deref() == Some("yes") {
        analysis.findings.push(Finding {
            message: "ChallengeResponseAuthentication is enabled".to_string(),
            severity: Severity::Warning,
        });
    }

    if protocol
        .as_deref()
        .map(|value| value.contains('1'))
        .unwrap_or(false)
    {
        analysis.findings.push(Finding {
            message: "SSH protocol version 1 is allowed".to_string(),
            severity: Severity::Critical,
        });
    }

    analysis
}

fn analyze_sudoers(path: &Path) -> Result<SudoersAnalysis> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(analyze_sudoers_from_str(&contents))
}

fn analyze_sudoers_from_str(contents: &str) -> SudoersAnalysis {
    let mut analysis = SudoersAnalysis {
        includes_dir: contents.lines().any(|line| line.contains("#includedir")),
        findings: Vec::new(),
    };

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.contains("NOPASSWD:") && line.contains("ALL") {
            analysis.findings.push(Finding {
                message: format!("Potential password-less sudo entry: {}", line),
                severity: Severity::Warning,
            });
        }

        if line.contains("ALL=(ALL) ALL") && line.split_whitespace().next() == Some("ALL") {
            analysis.findings.push(Finding {
                message: "Wildcard sudo entry grants full access".to_string(),
                severity: Severity::Critical,
            });
        }
    }

    analysis
}

fn analyze_cgroups() -> CgroupAnalysis {
    let unified_path = Path::new("/sys/fs/cgroup");
    let controllers_path = unified_path.join("cgroup.controllers");
    let mut analysis = CgroupAnalysis {
        unified_hierarchy: controllers_path.exists(),
        controllers: Vec::new(),
        findings: Vec::new(),
    };

    if analysis.unified_hierarchy {
        if let Ok(contents) = fs::read_to_string(&controllers_path) {
            analysis.controllers = contents.split_whitespace().map(|s| s.to_string()).collect();
        }
    } else {
        analysis.findings.push(Finding {
            message: "Host is not running with cgroup v2 unified hierarchy".to_string(),
            severity: Severity::Warning,
        });
    }

    analysis
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sshd_analysis_detects_insecure_settings() {
        let config = r#"
# Comment line
PasswordAuthentication yes
PermitRootLogin yes
ChallengeResponseAuthentication yes
Protocol 2,1
        "#;

        let analysis = analyze_sshd_config_from_str(config);
        assert_eq!(analysis.findings.len(), 4);
        assert!(
            analysis
                .findings
                .iter()
                .any(|f| f.severity == Severity::Critical)
        );
    }

    #[test]
    fn sshd_analysis_marks_hardening() {
        let config = r#"
KexAlgorithms curve25519-sha256
        "#;
        let analysis = analyze_sshd_config_from_str(config);
        assert!(analysis.hardening_present);
        assert!(analysis.findings.is_empty());
    }

    #[test]
    fn sudoers_analysis_detects_wildcard() {
        let sudoers = "ALL    ALL=(ALL) ALL";
        let analysis = analyze_sudoers_from_str(sudoers);
        assert_eq!(analysis.findings.len(), 1);
        assert_eq!(analysis.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn sudoers_analysis_detects_nopasswd() {
        let sudoers = "%wheel ALL=(ALL) NOPASSWD: ALL";
        let analysis = analyze_sudoers_from_str(sudoers);
        assert_eq!(analysis.findings.len(), 1);
        assert_eq!(analysis.findings[0].severity, Severity::Warning);
    }
}
