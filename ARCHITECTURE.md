# VMIC Architecture Plan (Consolidated)

VMIC is a modular Rust tool that produces human- and machine-readable system reports from Linux hosts. It ships as a single statically linked binary built against musl, collects data via compile-time registered modules (OS, /proc, journald, Docker, and planned extensions), and renders Markdown/JSON outputs for operators.

**Legend:** ✅ completed · ⚙️ in progress · ⏳ planned · 💤 deferred

## 1. Goals and Principles
- ✅ Produce a single statically linked binary (`x86_64-unknown-linux-musl`) with graceful degradation when data sources are missing.
- ✅ Provide both human-readable (Markdown) and machine-readable (JSON) output via compile-time Askama templates.
- ✅ Operate without root by default; continue to degrade politely when privileged resources are unavailable.

## 2. Workspace Structure
- ✅ Cargo workspace with shared profiles, lints, and dependencies (`Cargo.toml`, `.cargo/config.toml`).
- ✅ Workspace uses Rust 2024 edition, resolver = "3", `rust-version = 1.85`, and shared `[workspace.dependencies]` (`anyhow`, `thiserror`, `serde`, `serde_json`, `clap`, `askama`, `inventory`, `procfs`, `etc-os-release`, `rustix`, `tokio`/`bollard` behind features).
- ✅ Crates: `vmic-cli` (binary), `vmic-core` (orchestration and rendering), `vmic-sdk` (collector SDK), `modules/*` (feature crates), `templates/` (render assets).

## 3. SDK and Collector Registry
- ✅ Trait-based collectors with compile-time registration using `inventory` (`inventory::submit!`).
- ✅ Helper macros and section helpers (`success/degraded/error`) for unified output.

## 4. Core Runtime & Rendering
- ✅ Markdown rendering (Askama) and JSON serialization with metadata (timestamp, section count); Askama configured via crate-local `askama.toml`; the embedded JSON schema (`schemas/vmic-report.schema.json`) is consumed through `vmic-core/src/schema.rs` and verified by unit tests.
- ⚙️ HTML renderer exposed via `--format html` renders a structured dashboard (tables/lists per section, health digest summary); enhancements like alternate themes or drill-down views remain open.

## 5. Command-Line Interface
- ✅ Clap-based CLI with Markdown/JSON/HTML modes; default build enables the `journal` feature and the Docker client is activated via the `mod-docker/client` feature flag.
- ✅ Builds default to the musl target with `crt-static` via `.cargo/config.toml`.
- ✅ Multi-format support (`--format` accepts repeated values), output directory control (`--output-dir`), and relative time filtering (`--since`).

## 6. Modules
| Module | Scope | Status |
| --- | --- | --- |
| `mod-os` | `/etc/os-release`, `uname` | ✅ implemented |
| `mod-proc` | `/proc` load, memory, swap | ✅ host/cgroup memory, PSI, zram plus top offender drill-down and PSI sparkline |
| `mod-journal` | `journalctl --output=json` ingest | ✅ implemented |
| `mod-docker` | Docker API via `bollard` (`tokio` runtime, feature `client`) | ✅ engine/info, per-container metrics, health status, and storage sizing |
| `mod-users` | `/etc/passwd`, groups, sudo membership | ✅ Implemented |
| `mod-cron` | system crontab + `/etc/cron.d` | ✅ Implemented |
| `mod-services` | systemd unit status via `systemctl` | ✅ Implemented |
| `mod-network` | interfaces, sockets, listening ports | ✅ socket→PID/cgroup mapping with grouped host/container listener summaries and hardening insights |
| `mod-storage` | mounts, usage, heavy directories | ✅ operational vs pseudo FS split, inode tracking, Docker usage, heavy directory/log hotspots |
| `mod-sar` | sysstat historical metrics (feature) | ✅ implemented (CPU averages) |
| `mod-containers` | Podman/containerd (feature; e.g., `podman`, `containerd`) | ✅ implemented (runtime detection) |
| Security posture | sudoers, sshd_config, cgroups v2 | ✅ baseline checks implemented |

## 6.1 Cross-Module Health Digest
- ✅ Introduced a centralized "Critical Health Digest" in `vmic-core` that aggregates high-severity findings from all sections.
- ✅ Digest surfaces section errors/degradations automatically and flags module-specific alerts (e.g., disk usage >90%, low memory) using explicit rules.
- ✅ Exposed digest at the top of JSON/Markdown/HTML outputs with succinct severity badges.
- ✅ Allow operators to tune digest thresholds via CLI flags/env (
  - `storage.disk_warning`/`storage.disk_critical` usage ratios, default 90%/95%
  - `memory.warning`/`memory.critical` available-memory ratios, default 10%/5%
  ) while keeping sensible defaults documented.

## 7. Build, Testing, and Tooling
- ✅ Release profile tuned for size (`opt-level = "z"`, `lto = "thin"`, `panic = "abort"`, `strip = "symbols"`).
- ✅ Formatting via `cargo fmt`; unit tests per crate; smoke tests via `cargo run` documented.
- ✅ Release binary smoke-tested via `cargo build --release` followed by running `vmic --format json`.
- ⏳ CI automation (fmt/clippy/test matrix) intentionally deferred.

## 8. Future Enhancements
- ✅ Add HTML report template and aggregated HTML/JSON artifact generation.
- ✅ Define and publish a JSON schema for machine-readable reports.
- ✅ Extend Docker module with container metrics, graceful fallback when daemon unreachable — per-container health, sizes, limits, and runtime metadata delivered.
- ✅ Implement modular security checks (cgroups, sshd, sudoers) with baseline hardening rules.
- ✅ Disk usage drill-down (top directories/logs) for operational mounts.
- ⏳ HTML UX refinements (navigation, collapsible sections, badges, tooltips, pseudo-FS "Noise" area).
- ✅ Network insights: listener grouping, container correlation, service classification, and hardening heuristics.
- 💤 Investigate extended `sar` ingestion and cross-runtime container coverage when demand appears.

## 9. Additions Beyond Initial Plan
- Centralized health digest with tunable thresholds and cross-section synthesis (storage/memory rules) beyond minimal aggregation.
- HTML dashboard with structured section views (key-values, tables, lists, notes) and severity badges.
- Security posture checks: SSH and sudoers heuristics plus cgroup v2 detection (warnings surfaced as degraded findings).
- Storage drill-down: heavy directory and log hotspot sampling per operational mount; Docker storage breakdown (overlay, logs, volumes, total).
- Network listener insights: wildcard binding and insecure/legacy service classification with socket→process→container correlation.
- Journald SSH activity summary (invalid users, auth failures, top users/hosts) derived from recent journal entries.
- Alternative container runtimes discovery (podman/nerdctl/ctr) for heterogeneous environments.
- CLI ergonomics: multi-output selection, deterministic artifact naming with UTC timestamps, and environment overrides for digest thresholds (`VMIC_DIGEST_*`).
