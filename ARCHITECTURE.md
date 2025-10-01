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
- ✅ Markdown rendering (Askama) and JSON serialization with metadata (timestamp, section count); Askama configured via crate-local `askama.toml`; JSON schema formalization is still pending.
- ⏳ HTML renderer (secondary template) and extended CLI flags (e.g., `--output html`, `--since`) remain to be implemented.

## 5. Command-Line Interface
- ✅ Clap-based CLI with Markdown/JSON modes; default build enables `journal` & `docker` modules while retaining feature flags for extensibility (`journal`, `docker`, module-specific feature toggles like `mod-docker/client`).
- ✅ Builds default to the musl target with `crt-static` via `.cargo/config.toml`.
- ⏳ Additional flags from original plan (`--since`, dual format generation, configurable output path) pending.

## 6. Modules
| Module | Scope | Status |
| --- | --- | --- |
| `mod-os` | `/etc/os-release`, `uname` | ✅ implemented |
| `mod-proc` | `/proc` load, memory, swap | ✅ implemented |
| `mod-journal` | `journalctl --output=json` ingest | ✅ implemented |
| `mod-docker` | Docker API via `bollard` (`tokio` runtime, feature `client`) | ⚙️ basic engine/info; error handling implemented; live stats still planned |
| `mod-users` | `/etc/passwd`, groups, shadow analysis | ⏳ |
| `mod-cron` | cron tabs, system timers | ⏳ |
| `mod-services` | init/systemd unit discovery (`systemctl`/D-Bus) | ⏳ |
| `mod-network` | interfaces, sockets, listening ports | ⏳ |
| `mod-storage` | mounts, usage, heavy directories | ⏳ |
| `mod-sar` | sysstat historical metrics (feature) | ⏳ |
| `mod-containers` | Podman/containerd (feature; e.g., `podman`, `containerd`) | ⏳ |
| Security posture | sudoers, sshd_config, cgroups v2 | 💤 future optional |

## 7. Build, Testing, and Tooling
- ✅ Release profile tuned for size (`opt-level = "z"`, `lto = "thin"`, `panic = "abort"`, `strip = "symbols"`).
- ✅ Formatting via `cargo fmt`; unit tests per crate; smoke tests via `cargo run` documented.
- ⏳ CI automation (fmt/clippy/test matrix) intentionally deferred.

## 8. Future Enhancements
- ⏳ Add HTML report template and aggregated HTML/JSON artifact generation.
- ⏳ Define and publish a JSON schema for machine-readable reports.
- ⏳ Extend Docker module with container metrics, graceful fallback when daemon unreachable.
- ⏳ Implement modular security checks (cgroups, sshd, sudoers) once core modules are stable.
- 💤 Investigate `sar` ingestion and cross-platform container runtimes when demand appears.

