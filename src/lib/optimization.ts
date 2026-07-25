import type { OptimizationFinding, OptimizationSummary } from './types';

const RULE_TITLES: Record<string, string> = {
  'repeated-read': 'Repeated identical read',
  'corrective-mutation': 'Mutation retry',
  'repeated-failure': 'Repeated failure',
  'high-tool-churn': 'High tool churn',
  'excessive-command-output': 'Oversized command output',
  // Kept for sessions produced by older analyzer versions.
  'tool-ratio-evidence': 'Tool mix context',
};

export function findingRuleTitle(ruleId: string): string {
  return RULE_TITLES[ruleId] ?? ruleId.replaceAll('-', ' ');
}

export function findingConfidence(finding: OptimizationFinding): string {
  if (finding.confidence) return finding.confidence;
  return finding.severity === 'warning' ? 'high' : 'medium';
}

export function findingOccurrences(finding: OptimizationFinding): number {
  return Math.max(1, finding.occurrences ?? 1);
}

export function findingAvoidableCalls(finding: OptimizationFinding): number {
  return Math.max(0, finding.avoidable_calls ?? 0);
}

export interface OptimizationFindingGroup {
  ruleId: string;
  title: string;
  severity: string;
  confidence: string;
  findings: OptimizationFinding[];
  occurrences: number;
  avoidableCalls: number;
}

const confidenceRank: Record<string, number> = { high: 3, medium: 2, low: 1 };

export function groupOptimizationFindings(findings: OptimizationFinding[]): OptimizationFindingGroup[] {
  const groups = new Map<string, OptimizationFindingGroup>();
  for (const finding of findings) {
    let group = groups.get(finding.rule_id);
    if (!group) {
      group = {
        ruleId: finding.rule_id,
        title: findingRuleTitle(finding.rule_id),
        severity: finding.severity,
        confidence: findingConfidence(finding),
        findings: [],
        occurrences: 0,
        avoidableCalls: 0,
      };
      groups.set(finding.rule_id, group);
    }
    group.findings.push(finding);
    group.occurrences += findingOccurrences(finding);
    group.avoidableCalls += findingAvoidableCalls(finding);
    if (finding.severity === 'warning') group.severity = 'warning';
    const confidence = findingConfidence(finding);
    if ((confidenceRank[confidence] ?? 0) > (confidenceRank[group.confidence] ?? 0)) {
      group.confidence = confidence;
    }
  }
  return [...groups.values()]
    .map((group) => ({
      ...group,
      findings: [...group.findings].sort((a, b) =>
        findingAvoidableCalls(b) - findingAvoidableCalls(a)
          || String(a.timestamp ?? '').localeCompare(String(b.timestamp ?? '')),
      ),
    }))
    .sort((a, b) =>
      Number(b.severity === 'warning') - Number(a.severity === 'warning')
        || b.avoidableCalls - a.avoidableCalls
        || b.findings.length - a.findings.length
        || a.title.localeCompare(b.title),
    );
}

export function summarizeOptimizationFindings(findings: OptimizationFinding[]): OptimizationSummary {
  const summary: OptimizationSummary = {
    findings: findings.length,
    warnings: 0,
    likely_avoidable_calls: 0,
    by_rule: {},
  };
  for (const finding of findings) {
    if (finding.severity === 'warning') summary.warnings += 1;
    summary.likely_avoidable_calls += findingAvoidableCalls(finding);
    summary.by_rule[finding.rule_id] = (summary.by_rule[finding.rule_id] ?? 0) + 1;
  }
  return summary;
}

export function optimizationSummaryOrCount(
  summary: OptimizationSummary | undefined,
  count: number,
): OptimizationSummary {
  return summary ?? { findings: count, warnings: 0, likely_avoidable_calls: 0, by_rule: {} };
}
