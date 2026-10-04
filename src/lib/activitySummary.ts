import type { ActivityDay, ActivityMetric, CalendarZone } from './calendarActivity';

export interface ActivitySummaryOptions {
  days: ActivityDay[];
  metric: ActivityMetric;
  zone: CalendarZone;
  harness: string | null;
  selectedProject: boolean;
  coverage: 'complete' | 'partial';
}

const escapeXml = (text: string) => text.replace(/[&<>"']/g, (value) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;' })[value]!);
const number = (value: number) => value.toLocaleString('en-US');

/** ponytail: reuse the calendar's measured daily facts; never serialize a
 * session/project object or add another query, price calculation, or renderer dependency. */
export function activitySummary(options: ActivitySummaryOptions): { svg: string; markdown: string } {
  const { days, metric, coverage } = options;
  if (!days.length || days.length > 366) throw new Error('Choose 1–366 measured calendar days.');
  const values = days.map((day) => day[metric]);
  if (values.some((value) => !Number.isSafeInteger(value) || value < 0)) throw new Error('Activity counts are unavailable.');
  const total = values.reduce((sum, value) => sum + value, 0);
  if (!Number.isSafeInteger(total)) throw new Error('Activity counts exceed the supported range.');
  const label = metric === 'tokens' ? 'tokens' : 'tool calls';
  const zone = options.zone === 'utc' ? 'UTC' : Intl.DateTimeFormat().resolvedOptions().timeZone;
  const provider = new Map([['codex', 'Codex'], ['claude_code', 'Claude Code'], ['gemini_cli', 'Gemini CLI']]).get(options.harness ?? '')
    ?? (options.harness ? 'Selected provider' : 'All providers');
  const scope = `${provider} · ${options.selectedProject ? 'selected project (name omitted)' : 'all projects'} · current session filters`;
  const first = days[0]; const last = days[days.length - 1];
  const bounds = `${new Date(first.from).toISOString()} — ${new Date(last.to).toISOString()}`;
  const evidence = coverage === 'partial' ? 'Partial recorded history; missing activity is unknown.' : 'Recorded history intact for this snapshot.';
  const caveat = 'Activity volume is not productivity, context size, or billed cost.';
  const months = [...new Set(days.map((day) => day.month))];
  const height = 210 + Math.ceil(months.length / 3) * 190;
  const text = (x: number, y: number, content: string, size = 13, weight = 'normal', fill = '#111827') => `<text x="${x}" y="${y}" font-size="${size}" font-weight="${weight}" fill="${fill}">${escapeXml(content)}</text>`;
  const maximum = Math.max(1, ...values);
  const colors = ['#e5e7eb', '#bbf7d0', '#86efac', '#4ade80', '#16a34a', '#166534'];
  const cells = months.map((month, monthIndex) => {
    const x = 24 + (monthIndex % 3) * 246;
    const y = 190 + Math.floor(monthIndex / 3) * 190;
    const monthDays = days.filter((day) => day.month === month);
    const week = ['S', 'M', 'T', 'W', 'T', 'F', 'S'].map((day, index) => text(x + index * 29 + 8, y + 23, day, 11)).join('');
    return text(x, y, month, 14, 'bold') + week + monthDays.map((day, index) => {
      const position = monthDays[0].weekday + index;
      const px = x + (position % 7) * 29;
      const py = y + 32 + Math.floor(position / 7) * 24;
      const value = day[metric];
      const level = value === 0 ? 0 : Math.max(1, Math.ceil(value / maximum * 5));
      return `<g><title>${escapeXml(`${day.date}: ${number(value)} recorded ${label}`)}</title><rect x="${px}" y="${py}" width="25" height="20" rx="3" fill="${colors[level]}"/>${text(px + 4, py + 14, String(day.day), 10, 'normal', level === 5 ? '#ffffff' : '#111827')}</g>`;
    }).join('');
  }).join('');
  const description = `${number(total)} recorded ${label}. ${scope}. Calendar timezone: ${zone}. Inclusive UTC range: ${bounds}. ${evidence} ${caveat}`;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="760" height="${height}" viewBox="0 0 760 ${height}" role="img" aria-labelledby="title description"><title id="title">Odometer recorded activity</title><desc id="description">${escapeXml(description)}</desc><rect width="760" height="${height}" fill="#ffffff"/><g fill="#111827" font-family="system-ui, sans-serif">${text(24, 34, 'Odometer · recorded activity', 22, 'bold')}${text(24, 65, `${number(total)} ${label} · ${first.date} — ${last.date}`, 19)}${text(24, 90, scope, 12)}${text(24, 112, `Calendar timezone: ${zone}`, 12)}${text(24, 134, `Inclusive UTC range: ${bounds}`, 11)}${text(24, 156, evidence, 12)}${cells}${text(24, height - 28, `Color is relative recorded ${label}; each cell includes its exact count.`, 12)}${text(24, height - 9, caveat, 12)}</g></svg>`;
  const markdown = `# Odometer recorded activity\n\n${number(total)} recorded ${label}.\n\n- Scope: ${scope}\n- Calendar timezone: ${zone}\n- Inclusive UTC range: ${bounds}\n- Coverage: ${evidence}\n\n${caveat}\n\nProject names, session identities, prompts, tool payloads, paths, and account identifiers are omitted.\n\n| Calendar date | Recorded ${label} |\n| --- | ---: |\n${days.map((day) => `| ${day.date} | ${number(day[metric])} |`).join('\n')}\n`;
  return { svg, markdown };
}
