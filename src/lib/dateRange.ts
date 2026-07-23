// Date-range presets and labelling shared by the filter pill and the
// analytics band, so both surfaces describe the same window the same way.

export type RangePreset = { label: string; from: () => Date | null };

function pad(n: number): string {
  return n.toString().padStart(2, '0');
}

/** Format a Date as a `datetime-local` input value (local time). */
export function toLocalInputValue(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function startOfToday(): Date {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d;
}

export const RANGE_PRESETS: RangePreset[] = [
  { label: 'All time', from: () => null },
  { label: 'Today', from: startOfToday },
  { label: 'Last 24h', from: () => new Date(Date.now() - 24 * 3600 * 1000) },
  { label: 'Last 7 days', from: () => new Date(Date.now() - 7 * 24 * 3600 * 1000) },
  { label: 'Last 30 days', from: () => new Date(Date.now() - 30 * 24 * 3600 * 1000) },
];

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function fmtBound(local: string): string {
  const d = new Date(local);
  if (isNaN(d.getTime())) return '…';
  return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
}

/** Human label for a `datetime-local` range: recognise the preset that
 *  produced the current bounds (within tolerance — presets are relative to
 *  "now"), else show the range. */
export function rangeLabelFor(dateFrom: string, dateTo: string): string {
  if (!dateFrom && !dateTo) return 'All time';
  if (dateFrom && !dateTo) {
    const fromMs = new Date(dateFrom).getTime();
    const tolerance = 90 * 1000; // presets round to the minute; allow drift
    for (const p of RANGE_PRESETS) {
      const d = p.from();
      if (d && Math.abs(fromMs - d.getTime()) < tolerance) return p.label;
    }
    return `${fmtBound(dateFrom)} – now`;
  }
  return `${dateFrom ? fmtBound(dateFrom) : '…'} – ${dateTo ? fmtBound(dateTo) : '…'}`;
}
