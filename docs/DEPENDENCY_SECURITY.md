# Rust dependency warning review

Reviewed October 4, 2026 for issue [#245](https://github.com/ekalb81/agent-odometer/issues/245). The starting audit was v0.8.20 (`d1f011d`). `cargo-audit 0.22.2` used RustSec database commit `ef6173cbc5c50ec8166f9a5b28f07834144373ee`, updated October 3. Warning kinds are informational classifications; they do not establish attacker-controlled reachability, and a build passing does not establish absence of exposure.

## Resolution blocker

The original `gix 0.86.0 -> gix-protocol 0.64.0 -> bisync ^0.3.0` path prevents an unlocked full Cargo resolution: both matching bisync releases are yanked. The existing locked build remains reproducible. Gix 0.87.1 replaces bisync/bisync_macros with gix-macros; existing [PR #222](https://github.com/ekalb81/agent-odometer/pull/222) supplies this scoped upgrade. No direct dirs major upgrade is required.

The application uses Gix locally for repository/worktree identity and bounded commit traversal (`git_outcomes.rs`, `project_identity.rs`, and the corresponding command paths). It does not enable Gix default features or introduce a Git network operation. Existing Git outcome and worktree/project identity tests cover these uses.

## Warning disposition

Incoming paths below summarize the relevant dependency families, rather than listing each repeated Tauri plugin edge. All-target Cargo trees include packages not compiled on the current host.

| Package / warning | Incoming path and supported use | Disposition |
| --- | --- | --- |
| `event-listener 5.4.1`, [RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221.html) | `tauri-plugin-opener -> zbus -> event-listener`, including async-broadcast/async-lock/async-process/event-listener-strategy. Linux among Odometer's supported platforms; absent from the Windows/macOS trees. | Update to 5.4.2, which corrects StackSlot Send/Sync bounds. The advisory requires a non-Send event tag crossing threads. async-lock uses the listener macro, but this review does not prove the triggering tag/thread combination is reachable. |
| `faster-hex 0.10.0`, [RUSTSEC-2026-0306](https://rustsec.org/advisories/RUSTSEC-2026-0306.html) | `gix -> gix-hash` and `gix-protocol -> gix-transport -> gix-packetline`, across platforms. The advisory concerns x86/x86_64 AVX2 decoding. | Update to 0.10.1. Reviewed Gix hash/prefix decoding uses checked `hex_decode`; its length guard precedes `hex_decode_unchecked`. This narrows those call sites' exposure but is not an exhaustive upstream reachability proof. |
| `glib 0.18.5`, [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) | Tauri/window/webview/menu/tray chain through gtk/atk/gdk/gio/cairo/pango/WebKitGTK. Linux runtime. | Residual: fixed in glib >=0.20, outside the GTK3 chain's 0.18 constraint. Adding a second glib version would leave the affected instance. Requires a compatible upstream GTK/Tauri migration or upstream backport. Odometer source has no direct VariantStrIter call; transitive runtime reachability remains unverified. Do not suppress the advisory. |
| `proc-macro-error 1.0.4`, [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) | `glib-macros -> glib` and `gtk3-macros -> gtk` in the Linux GTK/Tauri build chain. Macro/build dependency, not an application runtime API. | Residual unmaintained dependency. Replacement APIs such as manyhow or proc-macro2-diagnostics require changes in upstream macros; no compatible 1.x maintenance replacement is established. Track with the upstream GTK/Tauri migration. |
| `unic-ucd-ident 0.9.0`, [RUSTSEC-2025-0100](https://rustsec.org/advisories/RUSTSEC-2025-0100.html) | `tauri-utils -> urlpattern 0.3 -> unic-ucd-ident`; Tauri build/codegen/plugins and runtime across supported platforms. | Residual unmaintained Unicode implementation. Move through a compatible tauri-utils/urlpattern upgrade in routine Tauri maintenance (#246); do not replace Unicode tables locally. RustSec suggests ICU properties as an upstream alternative. |
| `unic-char-property 0.9.0`, [RUSTSEC-2025-0081](https://rustsec.org/advisories/RUSTSEC-2025-0081.html) | `unic-ucd-ident -> unic-char-property` in the same urlpattern/Tauri chain. | Same upstream Unicode disposition. |
| `unic-char-range 0.9.0`, [RUSTSEC-2025-0075](https://rustsec.org/advisories/RUSTSEC-2025-0075.html) | `unic-ucd-ident` and `unic-char-property -> unic-char-range`. | Same upstream Unicode disposition. |
| `unic-ucd-version 0.9.0`, [RUSTSEC-2025-0098](https://rustsec.org/advisories/RUSTSEC-2025-0098.html) | `unic-ucd-ident -> unic-ucd-version`. | Same upstream Unicode disposition. |
| `unic-common 0.9.0`, [RUSTSEC-2025-0080](https://rustsec.org/advisories/RUSTSEC-2025-0080.html) | `unic-ucd-version -> unic-common`. | Same upstream Unicode disposition. |
| `bisync 0.3.0`, yanked | Original Gix protocol dependency, present in the lockfile even with Gix default features disabled. | Removed by Gix 0.87.1 / PR #222; resolves the full-resolution blocker. |
| `chacha20 0.10.1`, yanked | `chacha20poly1305 0.11 -> chacha20`, exclusively the dev dependency used by the synthetic `sync_design_prototype.rs` integration test. | Update to non-yanked 0.10.2. No production encryption or sync feature is introduced. |

## Reproduction

Use Cargo's `--locked` flag for builds and tests. `cargo update --manifest-path src-tauri/Cargo.toml --dry-run` is the non-mutating resolver check; it can propose broad transitive changes, but those proposals are not authorization to apply them. The targeted remediation commands are:

```powershell
cargo update --manifest-path src-tauri/Cargo.toml -p event-listener --precise 5.4.2
cargo update --manifest-path src-tauri/Cargo.toml -p faster-hex --precise 0.10.1
cargo update --manifest-path src-tauri/Cargo.toml -p chacha20 --precise 0.10.2
```

Run cargo-audit against the resulting lockfile without advisory ignores. Read the vulnerability and warning sections separately, retain residual platform constraints, and verify the repository's six required checks and final-head CI/MSRV results. No app source, updater endpoint, capability, credential consent, or accounting contract needs to change for these targeted lock updates.

## Verified result

After the merged Gix and security fixes (#222 and #274), the three targeted warning updates leave zero vulnerability records, zero yanked packages, six unmaintained warnings, and the single Linux glib unsoundness warning in this database snapshot. A final full resolution dry run exits successfully; the lockfile SHA256 remains unchanged. The added defmt 0.3.100 entry is an optional faster-hex dependency and is not activated by this application's all-target dependency tree.

All six required local checks passed on Windows with Rust 1.98.1: frontend check, 273 Vitest plus 31 Node tests, frontend build, Rust formatting, strict locked Clippy, and 684 Rust tests (19 ignored). The existing Git-outcome and project-identity suites also passed for Gix 0.87.1. New warning patch MSRVs are event-listener 1.60, faster-hex 1.61, and chacha20 1.85, below the project's Rust 1.95 floor; final-head CI verifies the Linux build and declared floor. These checks establish compatibility, not exhaustive runtime reachability of upstream advisory paths.
