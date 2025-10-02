use once_cell::sync::Lazy;
use serde_json::Value;

/// JSON Schema describing the machine-readable VMIC report format (Draft 2020-12).
pub static REPORT_SCHEMA_JSON: &str = include_str!("../../schemas/vmic-report.schema.json");

/// Lazily parsed schema to make programmatic access ergonomic.
pub static REPORT_SCHEMA_VALUE: Lazy<Value> = Lazy::new(|| {
    serde_json::from_str(REPORT_SCHEMA_JSON)
        .expect("embedded VMIC report schema must be valid JSON")
});

/// Returns a borrowed reference to the parsed report schema as a `serde_json::Value`.
pub fn report_schema() -> &'static Value {
    &REPORT_SCHEMA_VALUE
}
