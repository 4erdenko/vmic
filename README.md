# VMIC CLI

The `vmic` binary produces system reports from the registered collectors in this workspace.  Reports can be rendered in Markdown, JSON, and HTML, and the CLI exposes options to tune time ranges, thresholds, and output locations.

## Quick start

- Build a static Linux binary (musl):
  - One-time: `rustup target add x86_64-unknown-linux-musl`
  - Build: `cargo build --release`
  - Binary: `target/x86_64-unknown-linux-musl/release/vmic`
- Run locally and print JSON: `cargo run -- --format json`
- Run with multiple formats to files: `vmic --format markdown,html --output-dir ./reports`

The workspace targets Rust 2024 (rustc 1.90+). Static linking is enforced via `.cargo/config.toml`.

## Supported collectors

VMIC ships modular collectors that each contribute a section to the report. Most degrade gracefully (status `degraded`) when a tool or permission is missing.

- `os` — Operating System: `/etc/os-release`, kernel release/version and machine.
- `proc` — Processes and Resources: load averages, host/cgroup memory, swap, zram, top processes.
- `storage` — Storage Overview: mounted filesystems, inode usage, Docker data-root summary, largest directories/logs.
- `network` — Network Overview: interface counters, listening sockets, process/container association and insights.
- `services` — System Services: `systemd` unit status summary (`systemctl`).
- `users` — Local Users: `/etc/passwd` and privileged group membership.
- `cron` — Scheduled Jobs: `/etc/crontab` and `/etc/cron.d` entries.
- `journal` — systemd journal: recent events (`journalctl`). Enabled by default through the `journal` feature.
- `docker` — Docker Containers: engine info, containers, metrics and storage breakdown (uses `bollard`).
- `containers` — Alternative Containers: presence and versions of `podman`, `nerdctl`, or `ctr`.
- `sar` — Sysstat Metrics: CPU averages from `sar -u 1 1`.

Notes and prerequisites:
- `journal` typically requires root or membership in the `systemd-journal` group.
- `services` requires `systemctl` (systemd-based hosts).
- `docker` requires access to the Docker daemon (root or `docker` group). When unavailable, the section degrades and reports an unavailable engine.
- `sar` requires the `sysstat` package; otherwise the section degrades with an explanatory note.

## Build from source

Static builds are the default:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release
# resulting binary:
./target/x86_64-unknown-linux-musl/release/vmic
```

Developer commands:
- `cargo test --workspace` — run all unit tests.
- `cargo clippy --workspace --all-targets -- -D warnings` — lint; treat warnings as errors.
- `cargo fmt` — format code (Rust 2024 edition).
- `cargo run -- --format json` — execute CLI locally.

## JSON schema

The JSON output conforms to `schemas/vmic-report.schema.json` and includes a top-level `metadata.health_digest` with an overall severity and individual findings.

## Usage

```bash
vmic [OPTIONS]
```

If no options are provided the tool prints a Markdown report to stdout.

## Output control

| Option | Description |
| --- | --- |
| `--format <fmt>[,<fmt>...]` (alias `--formats`) | One or more formats to generate. Accepted values: `markdown`, `json`, `html`. Defaults to `markdown`. Repeat the flag or provide a comma-separated list to emit multiple formats in one run. When multiple formats are requested, or when HTML is requested, the results are written to files. |
| `--output-dir <PATH>` | Directory where file outputs are stored. Defaults to the current working directory for formats that need files (`html` or multi-format runs). The directory is created if it does not exist. |

### Format behaviour

- **Markdown / JSON**
  - With a single format and no `--output-dir`, content is printed to stdout.
  - When multiple formats are requested or `--output-dir` is set, the artifact is saved as `vmic-report-<UTC timestamp>.md` / `.json` inside the output directory.
- **HTML**
  - Rendered as a human-friendly dashboard: key metrics appear as tables and bullet lists organised by section, the health digest sits at the top, and there is no raw JSON.
  - Always saved to `vmic-report-<UTC timestamp>.html` in the output directory (default: current directory).

Example:

```bash
vmic --format markdown,html --output-dir ./reports
```

Produces `reports/vmic-report-<timestamp>.md` and `.html`, and prints a confirmation for each file.

## HTML and Markdown templates

- Markdown is rendered with `templates/report.md` (Askama) and includes a critical health digest followed by JSON sections.
- HTML uses `templates/report.html` and renders a dashboard with a sticky header, table of contents, status coloring, and per-section summaries, notes, key-values, tables, and lists. HTML is always written to a file.

## Time filtering

| Option | Description |
| --- | --- |
| `--since <SINCE>` | Passes the value as `--since` to collectors that support it (currently the journald module). Accepts any string understood by `journalctl`, for example `"-2h"`, `"yesterday"`, or an RFC 3339 timestamp (`"2025-01-01T00:00:00Z"`). |

Example:

```bash
vmic --since "-6h" --format json
```

## Health digest thresholds

You can tune the global health digest without recompiling:

| Option | Default | Description |
| --- | --- | --- |
| `--digest-disk-warning <PERCENT>` | `90` | Warn when a mount exceeds this percentage of capacity. Accepts percentages (e.g. `85`) or ratios (e.g. `0.85`). |
| `--digest-disk-critical <PERCENT>` | `95` | Flag a mount as critical when usage meets or exceeds this percentage/ratio. |
| `--digest-memory-warning <PERCENT>` | `10` | Warn when available memory drops below this percentage of total. |
| `--digest-memory-critical <PERCENT>` | `5` | Flag available memory as critical below this percentage of total. |

The same thresholds can be set with environment variables prior to execution (CLI flags take precedence):

- `VMIC_DIGEST_DISK_WARNING`
- `VMIC_DIGEST_DISK_CRITICAL`
- `VMIC_DIGEST_MEMORY_WARNING`
- `VMIC_DIGEST_MEMORY_CRITICAL`

Values support either `0-100` (percent) or `0.0-1.0` (ratio) ranges.

Example:

```bash
VMIC_DIGEST_DISK_WARNING=80 VMIC_DIGEST_DISK_CRITICAL=90 \
  vmic --format markdown,json --output-dir ./reports
```

## Feature flags

- `journal` — enables the journald collector (default). To build without it: `cargo build --no-default-features`.

## Permissions and platforms

- Linux hosts are supported (uses `/proc`, `systemd` tools, and musl static linking).
- Some collectors require elevated permissions (e.g., `journal`, `docker`). When permissions are insufficient, sections degrade with explanatory notes.

## Exit status

`vmic` returns a non-zero exit code if any collector fails catastrophically (for example when the binary cannot execute `journalctl`).

## Help

Use `vmic --help` to print the up-to-date usage generated by `clap`.

## Release automation

Releases are orchestrated by [release-please](https://github.com/googleapis/release-please) and [cargo-dist](https://github.com/axodotdev/cargo-dist):

- Pushes to `master` with conventional commits keep a release PR open. Merging that PR tags the repository (tag format `vX.Y.Z`) and creates a GitHub Release. Configure a personal access token (`RELEASE_PLEASE_TOKEN`) with `contents:write` permissions in the repository secrets so the tag push triggers downstream workflows.
- Tag pushes (`v*`) run the cargo-dist workflow, producing `x86_64-unknown-linux-musl` archives in `target/dist` and uploading them to the GitHub Release alongside checksums.
- To dry-run the pipeline locally, install `cargo-dist` and execute `cargo dist plan`.

Forcing a specific version:
- Add a commit included in the next release with the footer `release-as: X.Y.Z` (for example, commit message body contains `release-as: 0.2.3`). release-please will set Cargo.toml, manifest, the tag and Release to that version.
