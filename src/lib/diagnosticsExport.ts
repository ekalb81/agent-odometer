// Redaction and JSON formatting for the provider-diagnostics bug-report
// export (issue #39). Pure, IPC-free transforms so they are unit testable in
// isolation from the Rust backend and the Diagnostics UI.
//
// Redaction is the default: `buildDiagnosticsExport`/`diagnosticsExportJson`
// strip exact configured root paths unless the caller explicitly opts in
// with `includePaths: true`. Nothing else in a `DiagnosticsReport` ever
// carries a path, prompt, tool output, or other sensitive content — counts,
// booleans, capability flags, model name strings, and reason text are all
// generic and safe to paste into a public bug report as-is.

import type { DiagnosticRoot, DiagnosticsReport, ProviderDiagnostic } from './types';

export interface DiagnosticsExportOptions {
  /** Include exact configured/default root paths. Off by default; this is
   *  the explicit local-only opt-in the export UI gates behind a checkbox. */
  includePaths?: boolean;
}

function redactRoot(root: DiagnosticRoot, index: number, includePaths: boolean): DiagnosticRoot {
  if (includePaths) return root;
  return { ...root, path: `<${root.kind}-root-${index + 1}>` };
}

function redactProvider(provider: ProviderDiagnostic, includePaths: boolean): ProviderDiagnostic {
  return {
    ...provider,
    roots: provider.roots.map((root, index) => redactRoot(root, index, includePaths)),
  };
}

/** Builds the redacted (by default) report object suitable for export. */
export function buildDiagnosticsExport(
  report: DiagnosticsReport,
  options: DiagnosticsExportOptions = {},
): DiagnosticsReport {
  const includePaths = options.includePaths ?? false;
  return {
    ...report,
    providers: report.providers.map((provider) => redactProvider(provider, includePaths)),
  };
}

/** Pretty-printed JSON string for clipboard copy or file export. */
export function diagnosticsExportJson(
  report: DiagnosticsReport,
  options: DiagnosticsExportOptions = {},
): string {
  return JSON.stringify(buildDiagnosticsExport(report, options), null, 2);
}
