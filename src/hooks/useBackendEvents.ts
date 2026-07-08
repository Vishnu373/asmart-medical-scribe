import { useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  onCorrectionSuggestion,
  onError,
  onGenerationToken,
  onInputLevel,
  onStateChanged,
  onTranscriptSegment,
} from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Subscribes the backend → UI events (§9.5) into the store, once, at the app
 * root. The views then read state reactively and never touch the event layer
 * directly.
 */
export function useBackendEvents() {
  const setRecordingState = useAppStore((s) => s.setRecordingState);
  const setInputLevel = useAppStore((s) => s.setInputLevel);
  const addSegment = useAppStore((s) => s.addSegment);
  const appendStreamingToken = useAppStore((s) => s.appendStreamingToken);
  const addSuggestion = useAppStore((s) => s.addSuggestion);
  const pushToast = useAppStore((s) => s.pushToast);

  useEffect(() => {
    let active = true;
    const unsubs: UnlistenFn[] = [];

    Promise.all([
      onStateChanged((p) => {
        setRecordingState(p.state);
        // Stale levels would freeze the meter at the last buckets once recording
        // ends; reset to a flat baseline whenever we leave RECORDING.
        if (p.state !== "RECORDING") setInputLevel([]);
      }),
      onInputLevel((p) => setInputLevel(p.level)),
      onTranscriptSegment((p) => addSegment(p)),
      onGenerationToken((p) => appendStreamingToken(p.text)),
      // Post-ASR correction (§6.7): each streamed record joins the pending list;
      // the terminal done/error just returns the machine to IDLE (via state-changed),
      // which is the additive, non-blocking behavior — no toast on failure.
      onCorrectionSuggestion((p) => addSuggestion(p)),
      onError((p) => pushToast(p.message, "error")),
    ]).then((fns) => {
      // If the component unmounted before the listeners registered, drop them.
      if (active) unsubs.push(...fns);
      else fns.forEach((f) => f());
    });

    return () => {
      active = false;
      unsubs.forEach((f) => f());
    };
  }, [setRecordingState, setInputLevel, addSegment, appendStreamingToken, addSuggestion, pushToast]);
}
