import { useEffect, useRef } from "react";
import { updateTranscript } from "@/bridge";
import { useAppStore } from "@/state";

const SAVE_DEBOUNCE_MS = 600;

/**
 * Live, editable transcript (FR-5). During RECORDING the buffer mirrors the
 * ordered `transcript-segment` stream (§9.5); after stop the textarea owns it
 * and manual edits are debounced-saved via `update_transcript`. A save needs a
 * persisted record id, which only exists once `stop_recording` has returned —
 * so edits are saved post-stop, matching the record → stop → edit flow.
 */
export default function TranscriptEditor() {
  const transcript = useAppStore((s) => s.transcript);
  const setTranscript = useAppStore((s) => s.setTranscript);
  const currentRecordId = useAppStore((s) => s.currentRecordId);
  const recordingState = useAppStore((s) => s.recordingState);
  const pushToast = useAppStore((s) => s.pushToast);

  // While RECORDING, segments keep rebuilding `transcript`; accepting edits would
  // let an incoming segment overwrite the clinician's keystrokes. Editing is only
  // allowed once streaming has stopped (record → stop → edit flow, FR-5).
  const readOnly = recordingState === "RECORDING";

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The not-yet-saved edit, so unmount can flush it instead of dropping it.
  const pending = useRef<{ id: string; text: string } | null>(null);

  const save = (id: string, text: string) => {
    pending.current = null;
    updateTranscript(id, text).catch((e) => pushToast(String(e), "error"));
  };

  useEffect(
    () => () => {
      // Flush a pending save on unmount (e.g. switching away from Recording
      // within the debounce window) so the last edit is never lost (NFR-8).
      if (timer.current) clearTimeout(timer.current);
      if (pending.current) save(pending.current.id, pending.current.text);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const onChange = (text: string) => {
    setTranscript(text);
    if (!currentRecordId) return; // no record to persist into yet
    pending.current = { id: currentRecordId, text };
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => save(currentRecordId, text), SAVE_DEBOUNCE_MS);
  };

  return (
    <textarea
      aria-label="Transcript"
      value={transcript}
      onChange={(e) => onChange(e.target.value)}
      readOnly={readOnly}
      placeholder="The live transcript appears here as you record."
      className="flex-1 resize-none rounded-md border border-neutral-800 bg-neutral-900 p-3 text-sm leading-relaxed text-neutral-100 placeholder:text-neutral-600 read-only:text-neutral-300 focus:border-neutral-600 focus:outline-none"
    />
  );
}
