import type { AppState, LlmStatus } from "@/bridge";

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

/**
 * Current status label. While IDLE it surfaces note-model readiness (§8.2 startup
 * fix) — `Loading` while the co-resident preload warms the model, then `Ready` once
 * loaded — so the sequence reads Idle → Loading → Ready → Recording → … Any active
 * state (Recording/Processing/…) always takes precedence over the model status, and
 * a "Paused" override applies during a paused recording (no backend PAUSED state).
 */
export default function StatusBadge({
  state,
  paused,
  llmStatus,
}: {
  state: AppState;
  paused: boolean;
  llmStatus: LlmStatus;
}) {
  const recording = state === "RECORDING";

  let label: string;
  let dot: string;
  let pulse = false;
  if (recording && paused) {
    label = "Paused";
    dot = "bg-amber-400";
  } else if (state === "IDLE" && llmStatus === "loading") {
    label = "Loading";
    dot = "bg-amber-400";
    pulse = true;
  } else if (state === "IDLE" && llmStatus === "ready") {
    label = "Ready";
    dot = "bg-emerald-500";
  } else {
    label = LABELS[state];
    dot = DOT[state];
    pulse = recording;
  }

  return (
    <span
      role="status"
      className="inline-flex items-center gap-2 rounded-full bg-neutral-800/60 px-3 py-1 text-sm text-neutral-200"
    >
      <span className={`h-2 w-2 rounded-full ${dot} ${pulse ? "animate-pulse" : ""}`} />
      {label}
    </span>
  );
}
