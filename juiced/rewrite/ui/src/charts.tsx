export function Sparkline({ values, max = 100, label, height = 40 }: { values: (number | null)[]; max?: number; label: string; height?: number }) {
  const width = 240;
  const known = values.map((value, index) => [index, value] as const).filter((point): point is readonly [number, number] => point[1] !== null);
  const top = Math.max(max, ...known.map(([, value]) => value)) || 1;
  const x = (index: number) => values.length <= 1 ? width : (index / (values.length - 1)) * width;
  const y = (value: number) => height - 2 - (value / top) * (height - 4);
  const line = known.map(([index, value], at) => `${at ? "L" : "M"}${x(index).toFixed(1)},${y(value).toFixed(1)}`).join(" ");
  const area = known.length > 1 ? `${line} L${x(known.at(-1)![0]).toFixed(1)},${height} L${x(known[0]![0]).toFixed(1)},${height} Z` : "";
  return <svg className="sparkline" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" role="img" aria-label={label}>
    {area && <path d={area} className="sparkline-area" />}
    {line && <path d={line} className="sparkline-line" vectorEffect="non-scaling-stroke" />}
  </svg>;
}

export function Meter({ value, label }: { value: number | null; label: string }) {
  const clamped = value === null ? 0 : Math.max(0, Math.min(100, value));
  return <div className="meter" role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={value ?? undefined}><span style={{ width: `${clamped}%` }} data-level={clamped > 90 ? "high" : clamped > 70 ? "mid" : "low"} /></div>;
}
