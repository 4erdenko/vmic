use anyhow::Result;
use clap::{Parser, ValueEnum};
use vmic_core::{Context, collect_report};

// Обязательные модули подключаем, чтобы регистрация прошла на этапе линковки.
use mod_os as _;
use mod_proc as _;

#[cfg(feature = "journal")]
use mod_journal as _;

use mod_docker as _;

#[derive(Parser, Debug)]
#[command(
    name = "vmic",
    version,
    about = "VMIC system report",
    author = "VMIC Team"
)]
struct Cli {
    /// Output format: markdown or json
    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Markdown,
    Json,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let context = Context::new();
    let report = collect_report(&context);

    match cli.format {
        OutputFormat::Markdown => {
            let rendered = report.to_markdown()?;
            println!("{}", rendered);
        }
        OutputFormat::Json => {
            let payload = report.to_json_value();
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    }

    Ok(())
}
