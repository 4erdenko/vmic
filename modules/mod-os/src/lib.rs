use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use etc_os_release::OsRelease;
use rustix::system::uname;
use serde_json::{Value, json};
use vmic_sdk::{CollectionContext, Collector, CollectorMetadata, Section, register_collector};

struct OsCollector;

impl Collector for OsCollector {
    fn metadata(&self) -> CollectorMetadata {
        CollectorMetadata {
            id: "os",
            title: "Операционная система",
            description: "Сведения из /etc/os-release и uname",
        }
    }

    fn collect(&self, _ctx: &CollectionContext) -> Result<Section> {
        let snapshot = build_snapshot().context("не удалось собрать сведения об ОС")?;
        Ok(section_from_snapshot(&snapshot))
    }
}

fn create_collector() -> Box<dyn Collector> {
    Box::new(OsCollector)
}

register_collector!(create_collector);

#[derive(Debug, Clone, PartialEq, Eq)]
struct OsSnapshot {
    pretty_name: String,
    name: String,
    version: Option<String>,
    version_id: Option<String>,
    id_like: Vec<String>,
    kernel_release: String,
    kernel_version: String,
    machine: String,
}

fn build_snapshot() -> Result<OsSnapshot> {
    let os = OsRelease::open().context("не удалось открыть /etc/os-release")?;
    let uname = uname();

    Ok(OsSnapshot {
        pretty_name: os.pretty_name().to_string(),
        name: os.name().to_string(),
        version: os.version().map(ToOwned::to_owned),
        version_id: os.version_id().map(ToOwned::to_owned),
        id_like: os
            .id_like()
            .map(|iter| iter.map(ToOwned::to_owned).collect())
            .unwrap_or_default(),
        kernel_release: to_string(uname.release()),
        kernel_version: to_string(uname.version()),
        machine: to_string(uname.machine()),
    })
}

fn section_from_snapshot(snapshot: &OsSnapshot) -> Section {
    let mut os_release: BTreeMap<&str, Value> = BTreeMap::new();
    os_release.insert("pretty_name", json!(snapshot.pretty_name));
    os_release.insert("name", json!(snapshot.name));

    if let Some(value) = &snapshot.version {
        os_release.insert("version", json!(value));
    }
    if let Some(value) = &snapshot.version_id {
        os_release.insert("version_id", json!(value));
    }
    if !snapshot.id_like.is_empty() {
        os_release.insert("id_like", json!(snapshot.id_like));
    }

    let body = json!({
        "os_release": os_release,
        "kernel": {
            "release": snapshot.kernel_release,
            "version": snapshot.kernel_version,
            "machine": snapshot.machine,
        }
    });

    let mut section = Section::success("os", "Операционная система", body);
    section.summary = Some(snapshot.summary());
    section
}

fn to_string(value: &std::ffi::CStr) -> String {
    value.to_string_lossy().to_string()
}

impl OsSnapshot {
    fn summary(&self) -> String {
        format!("{} (kernel {})", self.pretty_name, self.kernel_release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_includes_kernel_version() {
        let snapshot = OsSnapshot {
            pretty_name: "Test OS".into(),
            name: "test".into(),
            version: Some("1.0".into()),
            version_id: Some("1".into()),
            id_like: vec!["linux".into()],
            kernel_release: "5.0.0-test".into(),
            kernel_version: "#1 SMP".into(),
            machine: "x86_64".into(),
        };

        assert!(snapshot.summary().contains("5.0.0-test"));
    }

    #[test]
    fn section_contains_id_like_when_present() {
        let snapshot = OsSnapshot {
            pretty_name: "Test".into(),
            name: "test".into(),
            version: None,
            version_id: None,
            id_like: vec!["debian".into(), "ubuntu".into()],
            kernel_release: "6.1".into(),
            kernel_version: "#1".into(),
            machine: "aarch64".into(),
        };

        let section = section_from_snapshot(&snapshot);
        let os_release = section.body.get("os_release").unwrap();
        assert!(
            os_release
                .get("id_like")
                .and_then(|v| v.as_array())
                .is_some()
        );
    }
}
