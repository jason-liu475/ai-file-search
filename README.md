# AI File Search

AI File Search is a fast, safe, memory-conscious cross-platform local file search engine designed to become a reliable low-cost data entry point for AI tools.

The project starts with a Rust core and a CLI prototype before adding the desktop UI and AI-facing local API.

## Goals

- Fast local file-name search.
- Safe path handling and explicit permission boundaries.
- Low idle memory and CPU usage.
- Cross-platform architecture for Windows, macOS, and Linux.
- A local API that AI tools can use without bypassing user control.

## Current Status

Milestone 1 is being built: Rust workspace, core path model, scanner, index store, search, CLI, and benchmark fixtures.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

Apache-2.0. See [LICENSE](LICENSE).
