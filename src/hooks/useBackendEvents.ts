import { useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  onError,
  onGenerationToken,
  onInputLevel,
  onLlmStatus,
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
  const setLlmStatus = useAppStore((s) => s.setLlmStatus);
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
      // Note-model readiness (§8.2 startup fix): flip the "preparing" hint to ready,
      // or toast a preload failure (the first Generate still retries and re-surfaces it).
      onLlmStatus((p) => {
        setLlmStatus(p.status);
        if (p.status === "error" && p.message) pushToast(p.message, "error");
      }),
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
  }, [
    setRecordingState,
    setInputLevel,
    addSegment,
    appendStreamingToken,
    setLlmStatus,
    pushToast,
  ]);
}
