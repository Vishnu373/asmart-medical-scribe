import type { AppState } from "@/bridge";

const LABELS: Record<AppState, string> = {
  IDLE: "Idle",
  RECORDING: "Recording",
  PROCESSING: "Processing",
  CORRECTING: "Reviewing",
  GENERATING: "Generating",
};

const DOT: Record<AppState, string> = {
  IDLE: "bg-neutral-500",
  RECORDING: "bg-red-500",
  PROCESSING: "bg-amber-400",
  CORRECTING: "bg-sky-400",
  GENERATING: "bg-teal-400",
};

/** Current recording-state label, with a "Paused" override (no backend PAUSED state). */
export default function StatusBadge({ state, paused }: { state: AppState; paused: boolean }) {
  const recording = state === "RECORDING";
  const label = recording && paused ? "Paused" : LABELS[state];
  const dot = recording && paused ? "bg-amber-400" : DOT[state];

  return (
    <span
      role="status"
      className="inline-flex items-center gap-2 rounded-full bg-neutral-800/60 px-3 py-1 text-sm text-neutral-200"
    >
      <span
        className={`h-2 w-2 rounded-full ${dot} ${recording && !paused ? "animate-pulse" : ""}`}
      />
      {label}
    </span>
  );
}
