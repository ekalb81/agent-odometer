------------------------------ MODULE Boundary ------------------------------
EXTENDS Naturals

(* Independent bounded scenarios for the updater and rate delivery, local
   reporting and MCP setup, and the receipt hook. Each starts from its own
   Init branch. Signature verification, filesystem checks and byte-level
   serialization are represented by outcomes and field classes.
*)
VARIABLES scenario, release, local, hook
vars == <<scenario, release, local, hook>>

(* tauri.conf.json and capabilities/default.json, updater.svelte.ts,
   rates.rs load_at/save_at. A binary may install before the next process
   starts. The embedded card comes from the running app. *)
ReleaseInit == [stage |-> "idle", artifact |-> "none", clicked |-> FALSE,
                installed |-> 1, running |-> 1, active |-> "absent", backup |-> FALSE]

OfferUpdate(window, artifact) ==
    /\ window = "main" /\ release.stage = "idle"
    /\ release' = [release EXCEPT !.stage = "offered", !.artifact = artifact]

ChooseInstall(window) ==
    /\ window = "main" /\ release.stage = "offered"
    /\ release' = [release EXCEPT !.stage = "installing", !.clicked = TRUE]

FinishInstall ==
    /\ release.stage = "installing"
    /\ release' = IF release.artifact = "signed matching version"
                  THEN [release EXCEPT !.stage = "installed", !.installed = 2]
                  ELSE [release EXCEPT !.stage = "rejected"]

StartInstalledApp ==
    /\ release.stage = "installed"
    /\ release' = [release EXCEPT !.stage = "restarted", !.running = release.installed]

SaveRateOverride(valid, backupWritten) ==
    release' = IF valid THEN [release EXCEPT !.active = "valid override",
                                            !.backup = release.backup \/ backupWritten]
                       ELSE release

ExternalRateChange(source) == release' = [release EXCEPT !.active = source]

RateDelivery ==
    CASE release.active = "valid override" -> "validated local override"
      [] release.active = "invalid override" /\ release.backup -> "validated local backup"
      [] OTHER -> "embedded app bundle"

ReleaseNext ==
    \/ \E window \in {"main", "widget"} :
        \/ \E artifact \in {"signed matching version", "bad signature", "signed wrong version"} :
               OfferUpdate(window, artifact)
        \/ ChooseInstall(window)
    \/ FinishInstall \/ StartInstalledApp
    \/ \E valid, backupWritten \in BOOLEAN : SaveRateOverride(valid, backupWritten)
    \/ \E source \in {"absent", "invalid override"} : ExternalRateChange(source)

(* diagnosticsExport.ts, headless.rs, mcp_server.rs, and
   history_accounting_export.rs. Model fields, not contents of free-form
   labels. A title or project label can still identify private work. *)
NoReport == [kind |-> "none", outcome |-> "none", paths |-> FALSE, fields |-> {}]
LocalInit(ready) == [ready |-> ready, ledgerRevision |-> 1, configRevision |-> 1,
                     previewRevision |-> 0, mcpConfigured |-> FALSE, result |-> NoReport]
Kinds == {"diagnostics", "cli projects", "cli sessions", "mcp diagnostics", "mcp projects",
          "session export", "activity card"}
AllowsPaths == {"diagnostics", "cli projects", "session export"}
OptionalLedger == {"diagnostics", "mcp diagnostics"}

ReportFields(kind, paths) ==
    CASE kind \in OptionalLedger -> {"health", "coverage", IF paths THEN "configured root" ELSE "root placeholder"}
      [] kind \in {"cli projects", "mcp projects"} ->
            {"aggregate", "project key", IF paths THEN "project path" ELSE "redacted project label"}
      [] kind = "cli sessions" -> {"aggregate", "session key"}
      [] kind = "session export" -> {"aggregate", "session key", "session title"}
            \cup (IF paths THEN {"working directory"} ELSE {})
      [] OTHER -> {"aggregate", "coverage", "date"}

Report(kind, paths) ==
    LET outcome == IF paths /\ kind \notin AllowsPaths THEN "invalid request"
                   ELSE IF ~local.ready /\ kind \notin OptionalLedger THEN "unavailable"
                   ELSE "published"
    IN local' = [local EXCEPT !.result = [kind |-> kind, paths |-> paths, outcome |-> outcome,
                    fields |-> IF outcome = "published" THEN ReportFields(kind, paths) ELSE {}]]

(* mcp_integration.rs preview/apply and harness_integration.rs apply_mcp_config.
   A saved entry is only configuration evidence. Receipt observation is a
   separate scenario. One preview for one client and scope is sufficient to
   exercise consumption, expiry and a conflicting external edit. *)
PreviewMcp ==
    /\ ~local.mcpConfigured
    /\ local' = [local EXCEPT !.previewRevision = local.configRevision,
                               !.result = [kind |-> "MCP setup preview", paths |-> TRUE,
                                           outcome |-> "published",
                                           fields |-> {"config path", "managed entry", "setup warning"}]]

ExpirePreview ==
    /\ local.previewRevision # 0
    /\ local' = [local EXCEPT !.previewRevision = 0]

ExternalConfigEdit ==
    /\ local.configRevision = 1
    /\ local' = [local EXCEPT !.configRevision = 2]

ApplyMcp ==
    /\ local.previewRevision # 0
    /\ local' = IF local.previewRevision = local.configRevision
                THEN [local EXCEPT !.mcpConfigured = TRUE,
                                    !.configRevision = local.configRevision + 1,
                                    !.previewRevision = 0]
                ELSE [local EXCEPT !.previewRevision = 0]

LocalNext == \/ \E kind \in Kinds, paths \in BOOLEAN : Report(kind, paths)
             \/ PreviewMcp \/ ExpirePreview \/ ExternalConfigEdit \/ ApplyMcp

(* turn_receipts.rs try_run_cli/print_hook_output and harness_integration.rs
   build_harness_status. Model one provider with combined app/provider opt-in.
   A direct stale invocation after disable remains possible. Only an enabled
   invocation records run health. A failed run still allows the host to continue.
   Freshness is represented by the selected configuration generation.
*)
HookInit == [requested |-> FALSE, config |-> "missing", revision |-> 0,
             prefsFile |-> FALSE, pending |-> FALSE, startRevision |-> 0, strictlyLater |-> FALSE,
             lastRun |-> "none", runStrictlyLater |-> FALSE, runRevision |-> 0, observed |-> FALSE,
             host |-> "idle", effect |-> "none", checkedInput |-> FALSE]

EnableHook ==
    /\ hook.revision = 0
    /\ hook' = [hook EXCEPT !.requested = TRUE, !.prefsFile = TRUE, !.config = "current", !.revision = 1]

DisableHook(cleaned) ==
    /\ hook.requested
    /\ hook' = [hook EXCEPT !.requested = FALSE, !.observed = FALSE,
                           !.config = IF cleaned THEN "missing" ELSE hook.config]

ChangeHookConfig(config) ==
    /\ hook.revision = 1
    /\ hook' = [hook EXCEPT !.config = config, !.revision = 2, !.observed = FALSE]

StartHook(later) ==
    /\ ~hook.pending
    /\ hook' = IF hook.requested
               THEN [hook EXCEPT !.pending = TRUE, !.startRevision = hook.revision, !.strictlyLater = later]
               ELSE [hook EXCEPT !.host = "continue", !.effect = "none", !.prefsFile = hook.prefsFile]

FinishHook(validInput, completed) ==
    /\ hook.pending
    /\ LET success == validInput /\ completed
           fresh == hook.startRevision = hook.revision /\ hook.strictlyLater
       IN hook' = IF hook.requested
                  THEN [hook EXCEPT !.pending = FALSE, !.lastRun = IF success THEN "success" ELSE "failed",
                           !.runRevision = hook.startRevision, !.runStrictlyLater = hook.strictlyLater, !.checkedInput = validInput,
                           !.host = "continue", !.effect = "read and record health",
                           !.observed = success /\ fresh /\ hook.config = "current"]
                  ELSE [hook EXCEPT !.pending = FALSE, !.host = "continue", !.effect = "none"]

HookNext == \/ EnableHook
            \/ \E cleaned \in BOOLEAN : DisableHook(cleaned)
            \/ \E config \in {"missing", "current", "duplicate", "disabled"} : ChangeHookConfig(config)
            \/ \E later \in BOOLEAN : StartHook(later)
            \/ \E validInput, completed \in BOOLEAN : FinishHook(validInput, completed)

Init ==
    /\ scenario \in {"release", "local", "hook"}
    /\ release = ReleaseInit /\ hook = HookInit
    /\ IF scenario = "local" THEN \E ready \in BOOLEAN : local = LocalInit(ready)
       ELSE local = LocalInit(FALSE)

Next ==
    \/ /\ scenario = "release" /\ ReleaseNext /\ UNCHANGED <<scenario, local, hook>>
    \/ /\ scenario = "local" /\ LocalNext /\ UNCHANGED <<scenario, release, hook>>
    \/ /\ scenario = "hook" /\ HookNext /\ UNCHANGED <<scenario, release, local>>

Spec == Init /\ [][Next]_vars

SignedInstall ==
    /\ release.installed = 2 => release.clicked /\ release.artifact = "signed matching version"
    /\ release.running = 2 => release.installed = 2 /\ release.stage = "restarted"

RateSourceProvenance ==
    /\ (RateDelivery = "validated local override") <=> (release.active = "valid override")
    /\ (RateDelivery = "validated local backup") <=> (release.active = "invalid override" /\ release.backup)

NoReportLeaks ==
    LET result == local.result
        locationFields == {"configured root", "project path", "working directory"}
        excludedFields == {"prompt", "reply", "tool payload", "credentials", "private note", "full client config"}
    IN /\ result.fields \cap excludedFields = {}
       /\ result.fields \cap locationFields # {} => result.paths /\ result.kind \in AllowsPaths
       /\ "session title" \in result.fields => result.kind = "session export"
       /\ "config path" \in result.fields => result.kind = "MCP setup preview"
       /\ result.outcome # "published" => result.fields = {}

UsageReportNeedsLedger ==
    local.result.outcome = "published" /\ local.result.kind \in (Kinds \ OptionalLedger) => local.ready

DisabledInvokeIsReadOnly ==
    [][(~hook.requested /\ ~hook.requested') => UNCHANGED hook.prefsFile]_vars
ValidHookOutcome ==
    /\ hook.host \in {"idle", "continue"}
    /\ hook.lastRun = "success" => hook.checkedInput
    /\ hook.observed => hook.runStrictlyLater /\ hook.requested /\ hook.config = "current"
                                    /\ hook.lastRun = "success" /\ hook.runRevision = hook.revision

ExplicitInstallationAndRestart ==
    [][/\ release.installed' # release.installed => release.stage = "installing"
       /\ release.running' # release.running => release.stage = "installed"
       /\ release.clicked' # release.clicked => release.stage = "offered"]_vars

OverrideDoesNotInstallApp ==
    [][<<release.active, release.backup>>' # <<release.active, release.backup>> =>
       UNCHANGED <<release.installed, release.running>>]_vars

McpWriteRequiresCurrentPreview ==
    [][local.mcpConfigured' # local.mcpConfigured =>
       local.previewRevision = local.configRevision /\ local.previewRevision' = 0]_vars

ReportIsReadOnly ==
    [][/\ UNCHANGED local.ledgerRevision
       /\ local.result' # local.result => UNCHANGED <<local.configRevision, local.mcpConfigured>>]_vars

HookIsFailOpen ==
    [][/\ hook.host # "idle" => hook.host' = "continue"
       /\ hook.lastRun' # hook.lastRun => hook.requested /\ hook.host' = "continue"
       /\ ~hook.requested => UNCHANGED <<hook.lastRun, hook.runRevision, hook.checkedInput>>]_vars

=============================================================================
