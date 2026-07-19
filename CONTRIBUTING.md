# Contributing to Odometer

Thanks for your interest! Bug reports, feature ideas, and pull requests are all welcome.

## Reporting issues

Use the issue templates. For bugs, include your OS, Odometer version (title bar / release tag), and which harness (Codex / Claude Code) the problem involves. **Never paste real session-file contents** — they contain your prompts, replies, and local paths. If a parser bug needs sample data, redact it or construct a minimal synthetic line like the fixtures in `src-tauri/tests/fixtures/`.

## Development setup

Prerequisites: Node.js 22, stable Rust (MSRV 1.77), and the [Tauri 2 platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
npm ci
npm run tauri dev
```

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before touching the parsers or wire types — it documents the contracts and accounting invariants that are easy to break silently. [AGENTS.md](AGENTS.md) is the working agreement for changes (it's written for AI coding agents, but everything in it applies to humans too).

## Before opening a PR

Run the same checks CI runs:

```sh
npm run check
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Guidelines:

- Parser or accounting changes need focused tests with synthetic fixtures.
- Rust wire-struct changes must update the TypeScript mirrors in `src/lib/types.ts`.
- Keep both lockfiles committed; use `npm ci` and `--locked`.
- Don't broaden Tauri capabilities without a clear requirement.

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
