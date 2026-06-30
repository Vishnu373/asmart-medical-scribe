/**
 * Live mic-input meter (FR-12). Renders the spectrum buckets from `input-level`
 * (each 0..1) as vertical bars. Inactive when there's no signal.
 */
export default function LevelMeter({ level, active }: { level: number[]; active: boolean }) {
  const buckets = level.length > 0 ? level : Array(16).fill(0);

  return (
    <div
      role="meter"
      aria-label="Input level"
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={buckets.reduce((a, b) => Math.max(a, b), 0)}
      className="flex h-16 items-end gap-0.5"
    >
      {buckets.map((v, i) => (
        <div
          key={i}
          data-testid="level-bar"
          className={`w-1.5 rounded-sm transition-[height] duration-75 ${
            active ? "bg-teal-400" : "bg-neutral-700"
          }`}
          style={{ height: `${Math.max(2, Math.min(1, v) * 100)}%` }}
        />
      ))}
    </div>
  );
}
