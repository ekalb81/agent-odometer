# Controlled action contract (issue #46)

## Current boundary

Odometer may describe a proposed action, but it cannot apply or undo one through this contract. The shipped action surface is a local dry run. It does not install provider hooks, edit agent configuration, run shell commands, elevate permissions, or interpret a finding as consent. No `apply`, `undo`, or batch-execute IPC command is registered. A preview is not an audit entry or evidence of savings.

Enabling writes requires a separate security review and a new release decision. The review must cover each provider adapter and target path, its documented support, atomic backup/restore, interruption recovery, symlink and traversal defenses, concurrent edits, and emergency disable. #267's Git checkpoint proposal has the same preview and recovery concerns but does not authorize Git writes here.

## Versioned draft and preview

The request is a closed, versioned union. Its only proposed kinds are a guard for an existing advisory quota budget and a remediation associated with an existing workflow finding. Unknown fields, kinds, versions, oversized identifiers, and missing or stale source revisions fail closed. Neither kind accepts a command, arbitrary path, provider hook script, free-form replacement text, or URL.

Rust resolves each draft against its existing authority: the current quota configuration revision and budget ID, or a bounded workflow report and its finding lifecycle revision. The client never supplies the authoritative finding state or budget value. The preview returns a redacted target identity, the source revision, target type, observed preconditions, the intended class of change, backup and postcondition requirements, and an explicit `apply_available: false`. A missing adapter or unrecorded/stale finding is reported as unavailable. Preview generation performs no configuration or journal write.

The frontend may show the preview and the reason execution is unavailable. It must not expose a disabled button that implies an implementation exists. Budget amounts remain advisory until a provider-specific enforcement adapter is separately reviewed and enabled. A workflow rule is observational; resolving a finding or reducing tool calls does not establish accepted quality, causal savings, or a safe configuration edit.

## Disabled transaction engine and future review gate

An internal Rust transaction engine now exercises a bounded file replacement against synthetic allowlisted targets in tests. It publishes immutable metadata-only journal events atomically with action version, redacted identity, source revision, before/after SHA-256 hashes, result, and undo state. It keeps exact backup bytes in a separate private file, uses a same-directory temporary file for replacement, checks the target hash again immediately before replacement, reconciles interrupted prepared/replaced stages, and refuses undo after an external edit. A bounded batch reconciles the failing item, including a failure after replacement but before journal publication, then rolls earlier items back in reverse order. A rollback conflict leaves the external edit untouched and still attempts restoration of the other targets. Journal IDs are validated before constructing paths, and Unix journal directories require mode 0700 and current-user ownership.

The production entry points have a hardcoded disabled gate. No provider target adapter, write IPC, provider hook, retention policy, or live configuration mutation exists. The synthetic engine validates the transaction mechanics, not safe execution against a provider file. Portable rename plus a pre-write hash check still leaves a race with an external writer between the final check and replacement. A write-capable milestone needs a reviewed provider-specific lock or compare-and-swap strategy, private journal ownership and cleanup rules, recovery on startup before new writes, and adapter-specific tests before changing that gate.

No outcome is recorded as realized savings when a preview is generated or a future configuration write succeeds. Accepted delivery, user correction, and causal benefit require separate observed evidence under #45 and follow-on evaluation work.

## Review gates for a write-capable milestone

| Case | Required result before writes can be enabled |
| --- | --- |
| Unknown draft, stale revision, or renamed finding | Reject before target resolution. |
| Missing provider capability or denied confirmation | No target or journal mutation. |
| Symlink, traversal, target replacement, or concurrent edit | Reject at immediate pre-write revalidation. |
| Interrupted or partial batch write | Recover from exact backups and record each result. |
| Undo after an external edit | Refuse without overwriting the edit. |
| Per-session override or emergency disable | Show the active state and stop future enforcement immediately. |
| Full uninstall | Remove only owned artifacts and verify provider state. |

The internal engine's synthetic fixture tests cover atomic replacement, exact backup/undo, interrupted-stage recovery, symlink rejection on Unix, stale edits, and batch rollback. Each proposed provider adapter needs its own tests and security review before production writes can be enabled.
