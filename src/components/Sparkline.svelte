<script lang="ts">
  interface TokenHistoryPoint {
    timestamp: string;
    total_tokens: number;
  }

  interface Props {
    points: TokenHistoryPoint[];
    width?: number;
    height?: number;
  }

  let { points: rawPoints, width = 320, height = 56 }: Props = $props();

  // Large sessions carry thousands of history points; a polyline that dense
  // is visually identical to a few hundred points but far more expensive to
  // build and render. Stride-sample, always keeping the first and last point.
  const MAX_POINTS = 240;
  const points = $derived((() => {
    if (rawPoints.length <= MAX_POINTS) return rawPoints;
    const stride = (rawPoints.length - 1) / (MAX_POINTS - 1);
    const sampled: TokenHistoryPoint[] = [];
    for (let i = 0; i < MAX_POINTS - 1; i++) {
      sampled.push(rawPoints[Math.round(i * stride)]);
    }
    sampled.push(rawPoints[rawPoints.length - 1]);
    return sampled;
  })());

  const PAD_X = 6;
  const PAD_Y = 8;

  const innerW = $derived(width - PAD_X * 2);
  const innerH = $derived(height - PAD_Y * 2);

  const hasSeries = $derived(points.length >= 2);

  const times = $derived(points.map((p) => new Date(p.timestamp).getTime()));
  const tokens = $derived(points.map((p) => p.total_tokens));

  const tMin = $derived(hasSeries ? times[0] : 0);
  const tMax = $derived(hasSeries ? times[times.length - 1] : 1);
  const vMin = $derived(hasSeries ? Math.min(...tokens) : 0);
  const vMax = $derived(hasSeries ? Math.max(...tokens) : 1);
  // Avoid division-by-zero if all values identical.
  const vRange = $derived(vMax - vMin === 0 ? 1 : vMax - vMin);
  const tRange = $derived(tMax - tMin === 0 ? 1 : tMax - tMin);

  function scaleX(t: number): number {
    return PAD_X + ((t - tMin) / tRange) * innerW;
  }

  function scaleY(v: number): number {
    // SVG y-axis is inverted: larger values go up (lower y).
    return PAD_Y + innerH - ((v - vMin) / vRange) * innerH;
  }

  const polylinePoints = $derived(
    hasSeries
      ? times.map((t, i) => `${scaleX(t)},${scaleY(tokens[i])}`).join(' ')
      : '',
  );

  // Grid line at midpoint value.
  const midV = $derived(vMin + vRange / 2);
  const midY = $derived(scaleY(midV));

  // First and last screen coords for dot tooltips.
  const dotStart = $derived(
    hasSeries
      ? { x: scaleX(times[0]), y: scaleY(tokens[0]) }
      : null,
  );
  const dotEnd = $derived(
    hasSeries
      ? { x: scaleX(times[times.length - 1]), y: scaleY(tokens[tokens.length - 1]) }
      : null,
  );

  const numFmt = new Intl.NumberFormat();
</script>

<svg
  {width}
  {height}
  viewBox="0 0 {width} {height}"
  role="img"
  aria-label="Tokens over time sparkline"
  class="overflow-visible"
>
  {#if hasSeries}
    <!-- Faint mid-value grid line -->
    <line
      x1={PAD_X}
      y1={midY}
      x2={PAD_X + innerW}
      y2={midY}
      stroke="currentColor"
      stroke-opacity="0.12"
      stroke-width="1"
      stroke-dasharray="3 3"
    />

    <!-- Main sparkline -->
    <polyline
      points={polylinePoints}
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linejoin="round"
      stroke-linecap="round"
    />

    <!-- Start dot -->
    {#if dotStart}
      <circle
        cx={dotStart.x}
        cy={dotStart.y}
        r="3"
        fill="currentColor"
        stroke="none"
        opacity="0.7"
      >
        <title>{numFmt.format(tokens[0])} tokens · {new Date(points[0].timestamp).toLocaleString()}</title>
      </circle>
    {/if}

    <!-- End dot -->
    {#if dotEnd}
      <circle
        cx={dotEnd.x}
        cy={dotEnd.y}
        r="3.5"
        fill="currentColor"
        stroke="none"
      >
        <title>{numFmt.format(tokens[tokens.length - 1])} tokens · {new Date(points[points.length - 1].timestamp).toLocaleString()}</title>
      </circle>
    {/if}
  {:else}
    <!-- Flat line for single or empty data -->
    <line
      x1={PAD_X}
      y1={height / 2}
      x2={PAD_X + innerW}
      y2={height / 2}
      stroke="currentColor"
      stroke-opacity="0.3"
      stroke-width="1.5"
      stroke-dasharray="4 3"
    />
    <text
      x={width / 2}
      y={height / 2 + 16}
      text-anchor="middle"
      class="text-xs fill-current"
      opacity="0.4"
      font-size="10"
    >Single data point</text>
  {/if}
</svg>
