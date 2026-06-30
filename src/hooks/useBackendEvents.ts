import { useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { onError, onInputLevel, onStateChanged } from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Subscribes the backend → UI events (§9.5) into the store, once, at the app
 * root. The views then read state reactively and never touch the event layer
 * directly. `transcript-segment` and `generation-token` are wired by F3/F4 where
 * their accumulation logic lives; F2 needs state, input level and errors.
 */
export function useBackendEvents() {
  const setRecordingState = useAppStore((s) => s.setRecordingState);
  const setInputLevel = useAppStore((s) => s.setInputLevel);
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
  }, [setRecordingState, setInputLevel, pushToast]);
}
