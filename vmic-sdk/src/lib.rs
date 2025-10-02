use anyhow::Result;
use serde::Serialize;
use std::fmt;

/// Data collection context; can be extended with environment parameters.
#[derive(Debug, Default, Clone)]
pub struct CollectionContext {
    since: Option<String>,
}

impl CollectionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_since<S: Into<String>>(since: S) -> Self {
        Self {
            since: Some(since.into()),
        }
    }

    pub fn set_since<S: Into<String>>(&mut self, since: Option<S>) {
        self.since = since.map(|value| value.into());
    }

    pub fn since(&self) -> Option<&str> {
        self.since.as_deref()
    }
}

/// Collector metadata used for rendering and logging.
#[derive(Debug, Clone, Copy)]
pub struct CollectorMetadata {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

/// Section status describing success or degraded collection.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStatus {
    Success,
    Degraded,
    Error,
}

impl fmt::Display for SectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SectionStatus::Success => "success",
            SectionStatus::Degraded => "degraded",
            SectionStatus::Error => "error",
        };
        f.write_str(value)
    }
}

/// Result produced by a collector.
#[derive(Debug, Serialize)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub status: SectionStatus,
    pub summary: Option<String>,
    pub body: serde_json::Value,
    pub notes: Vec<String>,
}

impl Section {
    pub fn success(id: &'static str, title: &'static str, body: serde_json::Value) -> Self {
        Self {
            id,
            title,
            status: SectionStatus::Success,
            summary: None,
            body,
            notes: Vec::new(),
        }
    }

    pub fn degraded(
        id: &'static str,
        title: &'static str,
        summary: String,
        body: serde_json::Value,
    ) -> Self {
        Self {
            id,
            title,
            status: SectionStatus::Degraded,
            summary: Some(summary),
            body,
            notes: Vec::new(),
        }
    }

    pub fn error(id: &'static str, title: &'static str, error: String) -> Self {
        Self {
            id,
            title,
            status: SectionStatus::Error,
            summary: Some(error.clone()),
            body: serde_json::json!({ "error": error }),
            notes: Vec::new(),
        }
    }

    pub fn has_notes(&self) -> bool {
        !self.notes.is_empty()
    }
}

/// Common interface for data collection modules.
pub trait Collector: Send + Sync + 'static {
    fn metadata(&self) -> CollectorMetadata;
    fn collect(&self, ctx: &CollectionContext) -> Result<Section>;
}

/// Descriptor of a compile-time registry entry.
pub struct CollectorRegistration {
    pub constructor: fn() -> Box<dyn Collector>,
}

inventory::collect!(CollectorRegistration);

pub use inventory;

/// Helper macro to register a collector inside a module.
#[macro_export]
macro_rules! register_collector {
    ($ctor:expr) => {
        ::vmic_sdk::inventory::submit! {
            ::vmic_sdk::CollectorRegistration {
                constructor: $ctor,
            }
        }
    };
}

pub fn iter_registered_collectors() -> impl Iterator<Item = &'static CollectorRegistration> {
    inventory::iter::<CollectorRegistration>.into_iter()
}
