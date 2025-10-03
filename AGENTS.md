# Repository Guidelines

## Project Structure & Module Organization
- Workspace root defines shared configuration in `Cargo.toml` and `.cargo/config.toml`.
- CLI entry point lives in `vmic-cli/src/main.rs`; supporting crates: `vmic-core`, `vmic-sdk`.
- Modular collectors reside under `modules/` (`mod-os`, `mod-proc`, `mod-journal`, `mod-docker`).
- Templates for report rendering are stored in `templates/`.
- Unit tests sit alongside source files inside each crate (`*_tests` modules).

## Build, Test, and Development Commands
- `cargo build --release` — produces a static `target/x86_64-unknown-linux-musl/release/vmic` binary.
- `cargo run -- --format json` — runs the CLI locally and prints a JSON report.
- `cargo test --workspace` — executes all tests across the workspace modules.
- `cargo clippy --workspace --all-targets -- -D warnings` — run alongside linting and tests; treat every warning as a must-fix before submitting patches.
- `rustup target add x86_64-unknown-linux-musl` — one-time command to enable musl builds.

## Release Automation
- Release tagging is handled by release-please; ensure commits follow Conventional Commit style and keep the release PR green.
- Configure a repository secret `RELEASE_PLEASE_TOKEN` (PAT with `contents:write`) so release-please pushes tags that trigger downstream workflows.
- cargo-dist builds, packages, and uploads `x86_64-unknown-linux-musl` archives on tag pushes (`v*`) via `.github/workflows/release.yml`.

## Coding Style & Naming Conventions
- Rust 2024 edition, enforced via `cargo fmt`; run before submitting patches.
- Modules follow snake_case (`mod-os`), types use CamelCase (`OsSnapshot`), functions use snake_case.
- All user-facing text must be English; keep summaries concise and descriptive.

## Testing Guidelines
- Rely on the default Rust test framework (`#[cfg(test)]` modules within crates).
- Name tests for behavior under verification, e.g., `summary_includes_kernel_version`.
- Ensure new collectors include unit tests covering success/failure paths.
- Run `cargo test --workspace` prior to any push or pull request.

## Commit & Pull Request Guidelines
- Write commits in imperative mood (e.g., "Add Docker collector stub").
- Each PR should summarize scope, list affected modules, and mention test commands run.
- Link relevant issues in the description; attach sample CLI output when behavior changes.

## Security & Configuration Tips
- Store no secrets in this repo; runtime credentials must come from environment variables.
- The binary defaults to musl static linking; confirm `crt-static` flag remains in `.cargo/config.toml`.

## Agent Communication & Quality Rules
- Respond to users in the same language they use in their initial message.
- Every feature or fix must include accompanying tests that verify the behavior.
- After any change, run formatters and linters; resolve errors and flag contentious warnings or fix straightforward ones before completion.

- Cross-check progress against `ARCHITECTURE.md` after each change; mark completed/ongoing items there.
- Propose and record any new scope additions in `ARCHITECTURE.md` before implementation.
