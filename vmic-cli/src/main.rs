use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use vmic_core::{Context, DigestThresholds, collect_report_with_digest};

// Ensure mandatory modules are linked so their collectors register.
use mod_os as _;
use mod_proc as _;

#[cfg(feature = "journal")]
use mod_journal as _;

use mod_containers as _;
use mod_cron as _;
use mod_docker as _;
use mod_network as _;
use mod_sar as _;
use mod_services as _;
use mod_storage as _;
use mod_users as _;

#[derive(Parser, Debug)]
#[command(
    name = "vmic",
    version,
    about = "VMIC system report",
    author = "VMIC Team"
)]
struct Cli {
    /// Output format: markdown, json, or html
    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,

    /// Warn when any disk usage exceeds this percentage (default 90)
    #[arg(long, value_name = "PERCENT")]
    digest_disk_warning: Option<f64>,

    /// Mark as critical when any disk usage exceeds this percentage (default 95)
    #[arg(long, value_name = "PERCENT")]
    digest_disk_critical: Option<f64>,

    /// Warn when available memory falls below this percentage of total (default 10)
    #[arg(long, value_name = "PERCENT")]
    digest_memory_warning: Option<f64>,

    /// Mark as critical when available memory falls below this percentage of total (default 5)
    #[arg(long, value_name = "PERCENT")]
    digest_memory_critical: Option<f64>,
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Markdown,
    Json,
    Html,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let thresholds = load_thresholds(&cli)?;
    let context = Context::new();
    let report = collect_report_with_digest(&context, thresholds);

    match cli.format {
        OutputFormat::Markdown => {
            let rendered = report.to_markdown()?;
            println!("{}", rendered);
        }
        OutputFormat::Json => {
            let payload = report.to_json_value();
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        OutputFormat::Html => {
            let rendered = report.to_html()?;
            let timestamp = report
                .metadata
                .generated_at_utc()
                .unwrap_or_else(|| Utc::now());
            let file_name = format!(
                "vmic-report-{}.html",
                timestamp.format("%Y-%m-%dT%H-%M-%SZ")
            );
            let path = html_output_path(&file_name)?;
            fs::write(&path, rendered)?;
            println!("HTML report written to {}", path.display());
        }
    }

    Ok(())
}

fn html_output_path(file_name: &str) -> Result<PathBuf> {
    let mut dir = env::current_dir()?;
    dir.push(file_name);
    Ok(dir)
}

fn load_thresholds(cli: &Cli) -> Result<DigestThresholds> {
    let mut thresholds = DigestThresholds::default();

    apply_env_override("VMIC_DIGEST_DISK_WARNING", |ratio| {
        thresholds.disk_warning = ratio;
        Ok(())
    })?;
    apply_env_override("VMIC_DIGEST_DISK_CRITICAL", |ratio| {
        thresholds.disk_critical = ratio;
        Ok(())
    })?;
    apply_env_override("VMIC_DIGEST_MEMORY_WARNING", |ratio| {
        thresholds.memory_warning = ratio;
        Ok(())
    })?;
    apply_env_override("VMIC_DIGEST_MEMORY_CRITICAL", |ratio| {
        thresholds.memory_critical = ratio;
        Ok(())
    })?;

    if let Some(value) = cli.digest_disk_warning {
        thresholds.disk_warning = percent_to_ratio(value)?;
    }
    if let Some(value) = cli.digest_disk_critical {
        thresholds.disk_critical = percent_to_ratio(value)?;
    }
    if let Some(value) = cli.digest_memory_warning {
        thresholds.memory_warning = percent_to_ratio(value)?;
    }
    if let Some(value) = cli.digest_memory_critical {
        thresholds.memory_critical = percent_to_ratio(value)?;
    }

    thresholds.validate()?;
    Ok(thresholds)
}

fn apply_env_override<F>(key: &str, mut assign: F) -> Result<()>
where
    F: FnMut(f64) -> Result<()>,
{
    if let Ok(value) = env::var(key) {
        if !value.trim().is_empty() {
            let ratio = percent_str_to_ratio(&value)
                .with_context(|| format!("invalid value for {}", key))?;
            assign(ratio)?;
        }
    }
    Ok(())
}

fn percent_str_to_ratio(value: &str) -> Result<f64> {
    let parsed: f64 = value.trim().parse()?;
    percent_to_ratio(parsed)
}

fn percent_to_ratio(value: f64) -> Result<f64> {
    let ratio = if value > 1.0 { value / 100.0 } else { value };
    if !(0.0..=1.0).contains(&ratio) {
        anyhow::bail!("threshold must be between 0 and 100 (or 0.0-1.0)");
    }
    Ok(ratio)
}
