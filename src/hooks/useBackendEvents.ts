import { useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { onError, onInputLevel, onStateChanged, onTranscriptSegment } from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Subscribes the backend → UI events (§9.5) into the store, once, at the app
 * root. The views then read state reactively and never touch the event layer
 * directly. `generation-token` is wired by F4 where its accumulation logic lives.
 */
export function useBackendEvents() {
  const setRecordingState = useAppStore((s) => s.setRecordingState);
  const setInputLevel = useAppStore((s) => s.setInputLevel);
  const addSegment = useAppStore((s) => s.addSegment);
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
  }, [setRecordingState, setInputLevel, addSegment, pushToast]);
}
