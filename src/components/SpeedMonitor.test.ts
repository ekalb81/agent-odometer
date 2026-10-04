import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SpeedReport, SpeedSample } from '../lib/types';
import SpeedMonitor from './SpeedMonitor.svelte';

const { getSpeedReport, writeExport } = vi.hoisted(() => ({
  getSpeedReport: vi.fn(),
  writeExport: vi.fn(),
}));

vi.mock('../lib/ipc', () => ({ getSpeedReport, writeExport }));

function sample(overrides: Partial<SpeedSample> = {}): SpeedSample {
  return {
    completed_at: '2026-10-04T13:00:00.000Z',
    model: 'gpt-5.5',
    reasoning_effort: 'high',
    mode: 'fast',
    output_tokens: 100,
    reasoning_tokens: 20,
    duration_ms: 10_000,
    output_tps: 10,
    visible_tps: 8,
    time_to_first_token_ms: 500,
    timing_source: 'explicit',
    ...overrides,
  };
}

function report(rows: SpeedSample[], overrides: Partial<SpeedReport> = {}): SpeedReport {
  return {
    status: 'ready',
    reason: null,
    measurement: 'turn',
    source: 'codex_session_logs',
    generated_at: '2026-10-04T13:01:00.000Z',
    rows,
    excluded_count: 3,
    scanned_rows: rows.length + 3,
    truncated: false,
    ...overrides,
  };
}

beforeEach(() => {
  getSpeedReport.mockReset();
  writeExport.mockReset();
  writeExport.mockResolvedValue(true);
  Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
});

afterEach(() => {
  vi.useRealTimers();
});

describe('SpeedMonitor', () => {
  it('uses duration-weighted speed and keeps unknown mode out of Fast and Standard', async () => {
    getSpeedReport.mockResolvedValue(report([
      sample({ output_tokens: 100, duration_ms: 10_000, output_tps: 10 }),
      sample({ output_tokens: 900, duration_ms: 30_000, output_tps: 30 }),
      sample({ mode: 'unknown', model: 'gpt-5.4', output_tokens: 50, duration_ms: 5_000 }),
    ]));

    render(SpeedMonitor, { props: { active: true } });

    await waitFor(() => expect(screen.getByText('25.0 tok/s')).toBeTruthy());
    expect(screen.getByText('Fast (configured)').parentElement?.textContent).toContain('2');
    expect(screen.getByText('Fast (configured)').parentElement?.textContent).toContain('1,000');
    expect(screen.getByText('Unknown configured mode').parentElement?.textContent).toContain('1 turns');
    expect(screen.getByText('No configured Fast or Standard mode is inferred.')).toBeTruthy();
  });

  it('filters the latest and grouped rows and exports only the exact filtered sample fields', async () => {
    getSpeedReport.mockResolvedValue(report([
      sample({ completed_at: '2026-10-04T12:00:00.000Z', model: 'gpt-5.5' }),
      sample({ completed_at: '2026-10-04T13:00:00.000Z', model: 'gpt-5.4', reasoning_effort: 'low', mode: 'standard' }),
    ]));

    render(SpeedMonitor, { props: { active: true } });
    await screen.findByText('Comparable turn groups');

    await fireEvent.change(screen.getByLabelText('Speed report model'), { target: { value: 'gpt-5.4' } });
    await fireEvent.change(screen.getByLabelText('Speed report final effort'), { target: { value: 'low' } });
    expect(screen.getAllByText('standard').length).toBeGreaterThan(0);
    expect(screen.queryByText('fast')).toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: 'Export CSV' }));
    await waitFor(() => expect(writeExport).toHaveBeenCalledTimes(1));
    const [, format, csv] = writeExport.mock.calls[0];
    expect(format).toBe('csv');
    expect(csv).toContain('completed_at,model,reasoning_effort,mode,output_tokens,reasoning_tokens,duration_ms,output_tps,visible_tps,time_to_first_token_ms,timing_source,source,measurement');
    expect(csv).toContain('gpt-5.4,low,standard');
    expect(csv).toContain(',500,explicit,codex_session_logs,turn');
    expect(csv).not.toContain('gpt-5.5');
    expect(csv).not.toMatch(/prompt|path|thread/i);
  });

  it('renders and exports missing reasoning and visible-speed counts as unavailable', async () => {
    getSpeedReport.mockResolvedValue(report([
      sample({ reasoning_tokens: null, visible_tps: null }),
    ]));

    render(SpeedMonitor, { props: { active: true } });
    await screen.findByText('Latest turns (up to 20)');
    expect(screen.getAllByText('—', { exact: true }).length).toBeGreaterThanOrEqual(2);

    await fireEvent.click(screen.getByRole('button', { name: 'Export CSV' }));
    await waitFor(() => expect(writeExport).toHaveBeenCalledTimes(1));
    const csv = writeExport.mock.calls[0][2] as string;
    const cells = csv.trim().split(/\r?\n/)[1].split(',');
    expect(cells[5]).toBe('');
    expect(cells[8]).toBe('');
  });

  it('shows unavailable data and retries on request', async () => {
    getSpeedReport.mockResolvedValueOnce(report([], { status: 'unavailable', reason: 'No local Codex logs found' }));
    getSpeedReport.mockResolvedValueOnce(report([sample()]));

    render(SpeedMonitor, { props: { active: true } });
    expect(await screen.findByText(/Unavailable: No local Codex logs found/)).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(screen.getByText('Comparable turn groups')).toBeTruthy());
    expect(getSpeedReport).toHaveBeenCalledTimes(2);
  });

  it('preserves valid filters after refresh and displays distinct rows with equal timestamps', async () => {
    const sameTime = '2026-10-04T13:00:00.000Z';
    getSpeedReport.mockResolvedValue(report([
      sample({ completed_at: sameTime, model: 'gpt-5.4', reasoning_effort: 'low', mode: 'standard' }),
      sample({ completed_at: sameTime, model: 'gpt-5.5', reasoning_effort: 'high', mode: 'fast' }),
    ]));

    render(SpeedMonitor, { props: { active: true } });
    await screen.findByText('Comparable turn groups');
    const modelFilter = screen.getByLabelText('Speed report model') as HTMLSelectElement;
    await fireEvent.change(modelFilter, { target: { value: 'gpt-5.4' } });
    expect(modelFilter.value).toBe('gpt-5.4');

    await fireEvent.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(2));
    await waitFor(() => expect((screen.getByLabelText('Speed report model') as HTMLSelectElement).value).toBe('gpt-5.4'));
    expect(screen.getAllByText('gpt-5.4 · low').length).toBeGreaterThan(0);
    expect(screen.queryByText('gpt-5.5 · high')).toBeNull();
  });

  it('keeps a selected filter when refreshed rows no longer match and disables empty export', async () => {
    getSpeedReport
      .mockResolvedValueOnce(report([
        sample({ model: 'gpt-5.4', reasoning_effort: 'high' }),
        sample({ model: 'gpt-5.5', reasoning_effort: 'low' }),
      ]))
      .mockResolvedValueOnce(report([
        sample({ model: 'gpt-5.5', reasoning_effort: 'low' }),
      ]));

    render(SpeedMonitor, { props: { active: true } });
    await screen.findByText('Comparable turn groups');
    const modelFilter = screen.getByLabelText('Speed report model') as HTMLSelectElement;
    await fireEvent.change(modelFilter, { target: { value: 'gpt-5.4' } });
    await fireEvent.change(screen.getByLabelText('Speed report final effort'), { target: { value: 'high' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.getByText('No turns match these filters.')).toBeTruthy());

    expect((screen.getByLabelText('Speed report model') as HTMLSelectElement).value).toBe('gpt-5.4');
    expect((screen.getByLabelText('Speed report final effort') as HTMLSelectElement).value).toBe('high');
    expect(screen.getByRole('option', { name: 'gpt-5.4 (no current samples)' })).toBeTruthy();
    expect(screen.queryByText('gpt-5.5 · low')).toBeNull();
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeDisabled();
    expect(writeExport).not.toHaveBeenCalled();
  });

  it('defaults to turn throughput and replaces it with a separately queried response report', async () => {
    getSpeedReport
      .mockResolvedValueOnce(report([sample({ output_tokens: 300, duration_ms: 10_000, output_tps: 30 })]))
      .mockResolvedValueOnce(report(
        [sample({ output_tokens: 900, duration_ms: 10_000, output_tps: 90 })],
        { measurement: 'response', source: 'codex_local_logs' },
      ));

    render(SpeedMonitor, { props: { active: true } });
    await screen.findByText('Comparable turn groups');
    expect((screen.getByLabelText('Speed report measurement') as HTMLSelectElement).value).toBe('turn');
    expect(getSpeedReport.mock.calls[0][0].measurement).toBe('turn');
    expect(screen.getByText('30.0 tok/s')).toBeTruthy();

    await fireEvent.change(screen.getByLabelText('Speed report measurement'), { target: { value: 'response' } });
    await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(2));
    expect(getSpeedReport.mock.calls[1][0].measurement).toBe('response');
    await waitFor(() => expect(screen.getByText('90.0 tok/s')).toBeTruthy());
    expect(screen.queryByText('30.0 tok/s')).toBeNull();
    expect(screen.getByText('Fast (observed)')).toBeTruthy();
    expect(screen.getAllByText('Observed mode').length).toBeGreaterThan(0);
    expect(screen.getByText(/Mode shows the tier recorded for each response/)).toBeTruthy();
    expect(screen.getByText(/request latency/)).toBeTruthy();
  });

  it.each(['success', 'failure'] as const)(
    'ignores a stale measurement %s and queues the selected measurement report',
    async (staleOutcome) => {
      let finishStale!: (value: SpeedReport) => void;
      let failStale!: (error: Error) => void;
      getSpeedReport
        .mockImplementationOnce(() => new Promise<SpeedReport>((resolve, reject) => {
          finishStale = resolve;
          failStale = reject;
        }))
        .mockResolvedValueOnce(report(
          [sample({ output_tokens: 900, duration_ms: 10_000, output_tps: 90 })],
          { measurement: 'response', source: 'codex_local_logs' },
        ));

      render(SpeedMonitor, { props: { active: true } });
      await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(1));
      expect(getSpeedReport.mock.calls[0][0].measurement).toBe('turn');

      await fireEvent.change(screen.getByLabelText('Speed report measurement'), { target: { value: 'response' } });
      expect(getSpeedReport).toHaveBeenCalledTimes(1);
      if (staleOutcome === 'success') {
        finishStale(report([sample({ output_tokens: 300, duration_ms: 10_000, output_tps: 30 })]));
      } else {
        failStale(new Error('stale turn report failed'));
      }

      await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(2));
      expect(getSpeedReport.mock.calls[1][0].measurement).toBe('response');
      await waitFor(() => expect(screen.getByText('90.0 tok/s')).toBeTruthy());
      expect(screen.queryByText('30.0 tok/s')).toBeNull();
      expect(screen.queryByText(/stale turn report failed/)).toBeNull();
    },
  );

  it.each(['success', 'failure'] as const)(
    'queues the new window and ignores a stale %s from the previous request',
    async (staleOutcome) => {
      let finishStale!: (value: SpeedReport) => void;
      let failStale!: (error: Error) => void;
      getSpeedReport
        .mockImplementationOnce(() => new Promise<SpeedReport>((resolve, reject) => {
          finishStale = resolve;
          failStale = reject;
        }))
        .mockResolvedValueOnce(report([sample({ model: 'fresh-window' })]));

      render(SpeedMonitor, { props: { active: true } });
      await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(1));
      const oldRange = getSpeedReport.mock.calls[0][0];

      await fireEvent.change(screen.getByLabelText('Speed report window'), { target: { value: '7days' } });
      await waitFor(() => expect((screen.getByLabelText('Speed report window') as HTMLSelectElement).value).toBe('7days'));
      expect(getSpeedReport).toHaveBeenCalledTimes(1);

      if (staleOutcome === 'success') finishStale(report([sample({ model: 'stale-window' })]));
      else failStale(new Error('stale window failed'));

      await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(2));
      const newRange = getSpeedReport.mock.calls[1][0];
      expect(newRange.from).not.toBe(oldRange.from);
      expect(newRange.measurement).toBe('turn');
      await waitFor(() => expect(screen.getAllByText('fresh-window · high').length).toBeGreaterThan(0));
      expect(screen.queryByText('stale-window · high')).toBeNull();
      expect(screen.queryByText(/stale window failed/)).toBeNull();
    },
  );

  it('rejects a result after deactivation and stops polling while inactive', async () => {
    vi.useFakeTimers();
    let resolveFirst!: (value: SpeedReport) => void;
    getSpeedReport.mockImplementationOnce(() => new Promise<SpeedReport>((resolve) => { resolveFirst = resolve; }));

    const view = render(SpeedMonitor, { props: { active: true } });
    await waitFor(() => expect(getSpeedReport).toHaveBeenCalledTimes(1));
    await view.rerender({ active: false });
    resolveFirst(report([sample()]));
    await Promise.resolve();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(30_000);

    expect(getSpeedReport).toHaveBeenCalledTimes(1);
    expect(screen.queryByText('Comparable turn groups')).toBeNull();
  });
});
