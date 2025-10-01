use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;
use vmic_sdk::{self, CollectionContext, Section};

pub use vmic_sdk::{CollectionContext as Context, SectionStatus};

#[derive(Debug, Serialize)]
pub struct ReportMetadata {
    pub generated_at: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub metadata: ReportMetadata,
    pub sections: Vec<Section>,
}

impl Report {
    pub fn new(sections: Vec<Section>) -> Self {
        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".to_string());

        Self {
            metadata: ReportMetadata { generated_at },
            sections,
        }
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "metadata": {
                "generated_at": self.metadata.generated_at,
            },
            "sections": self.sections,
        })
    }

    pub fn to_markdown(&self) -> Result<String> {
        render::render_markdown(self).map_err(Into::into)
    }
}

pub fn collect_report(ctx: &CollectionContext) -> Report {
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

    Report::new(sections)
}

mod render {
    use askama::Template;

    use super::Report;

    #[derive(Template)]
    #[template(path = "report.md", escape = "none")]
    struct MarkdownReport<'a> {
        report: &'a Report,
    }

    pub fn render_markdown(report: &Report) -> askama::Result<String> {
        MarkdownReport { report }.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmic_sdk::SectionStatus;

    // Link modules so their collectors register during tests.
    use mod_docker as _;
    use mod_journal as _;
    use mod_os as _;
    use mod_proc as _;

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
    }

    #[test]
    fn markdown_render_contains_section_title() {
        let ctx = Context::new();
        let report = collect_report(&ctx);
        let md = report.to_markdown().expect("markdown render");
        assert!(md.contains("# System Report"));
    }
}
