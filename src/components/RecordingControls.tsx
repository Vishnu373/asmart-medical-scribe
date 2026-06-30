import {
  pauseRecording,
  resumeRecording,
  startRecording,
  stopRecording,
} from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Start / pause / resume / stop controls bound to the recording commands (§9.4).
 * The backend's state guards reject illegal transitions; a rejection surfaces as
 * an error toast. The button set is derived from the current state + paused flag.
 */
export default function RecordingControls() {
  const state = useAppStore((s) => s.recordingState);
  const paused = useAppStore((s) => s.paused);
  const setPaused = useAppStore((s) => s.setPaused);
  const setInputLevel = useAppStore((s) => s.setInputLevel);
  const setSegments = useAppStore((s) => s.setSegments);
  const setTranscript = useAppStore((s) => s.setTranscript);
  const setCurrentRecordId = useAppStore((s) => s.setCurrentRecordId);
  const pushToast = useAppStore((s) => s.pushToast);

  // Run a command; surface any rejection (the backend's `Err(String)`) as a toast.
  const run = async (fn: () => Promise<void>) => {
    try {
      await fn();
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  const onStart = () =>
    run(async () => {
      // Clear the previous session's transcript before a fresh consult.
      setSegments([]);
      setTranscript("");
      setCurrentRecordId(null);
      await startRecording();
      setPaused(false);
    });

  const onStop = () =>
    run(async () => {
      const id = await stopRecording();
      setCurrentRecordId(id);
      setPaused(false);
    });

  const onPause = () =>
    run(async () => {
      await pauseRecording();
      setPaused(true);
      // Pause emits no state-changed event (backend stays RECORDING), so clear
      // the levels here to flatten the meter while paused.
      setInputLevel([]);
    });

  const onResume = () =>
    run(async () => {
      await resumeRecording();
      setPaused(false);
    });

  const idle = state === "IDLE";
  const recording = state === "RECORDING";
  const busy = state === "PROCESSING" || state === "GENERATING";

  const base = "rounded-md px-4 py-2 text-sm font-medium transition-colors disabled:opacity-40";

  return (
    <div className="flex gap-2">
      {idle && (
        <button type="button" onClick={onStart} className={`${base} bg-teal-600 hover:bg-teal-500`}>
          Start recording
        </button>
      )}

      {recording && !paused && (
        <button
          type="button"
          onClick={onPause}
          className={`${base} bg-neutral-700 hover:bg-neutral-600`}
        >
          Pause
        </button>
      )}

      {recording && paused && (
        <button
          type="button"
          onClick={onResume}
          className={`${base} bg-neutral-700 hover:bg-neutral-600`}
        >
          Resume
        </button>
      )}

      {recording && (
        <button type="button" onClick={onStop} className={`${base} bg-red-600 hover:bg-red-500`}>
          Stop
        </button>
      )}

      {busy && (
        <button type="button" disabled className={`${base} bg-neutral-700`}>
          {state === "PROCESSING" ? "Processing…" : "Generating…"}
        </button>
      )}
    </div>
  );
}
