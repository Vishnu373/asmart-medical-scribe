import {
  cancelGeneration,
  generateNote,
  listNotes,
  regenerateNote,
  revertVersion,
} from "@/bridge";
import { useAppStore } from "@/state";
import SoapEditor from "@/components/SoapEditor";

/**
 * SOAP note generation + editing (§8.4–8.5, FR-6..FR-11). Generates from the
 * record's edited transcript, streams `generation-token` into a live view, then
 * shows the four-section editor for the active note plus its revertable version
 * history. Needs a saved record id, which only exists once `stop_recording` has
 * returned, so the panel is inert until there is one.
 */
export default function NotePanel() {
  const recordingState = useAppStore((s) => s.recordingState);
  const recordId = useAppStore((s) => s.currentRecordId);
  const notes = useAppStore((s) => s.notes);
  const streamingNote = useAppStore((s) => s.streamingNote);
  const setNotes = useAppStore((s) => s.setNotes);
  const setStreamingNote = useAppStore((s) => s.setStreamingNote);
  const pushToast = useAppStore((s) => s.pushToast);

  const generating = recordingState === "GENERATING";
  // Hold Generate until the §6.7 correction pass has ended (streamed/cancelled/
  // failed) — the machine leaves CORRECTING back to IDLE — preserving the
  // "sequenced, never concurrent" invariant on the UI side too.
  const correcting = recordingState === "CORRECTING";
  const active = notes.find((n) => n.is_active) ?? notes[0] ?? null;
  const ready = recordId !== null && !generating && !correcting;

  const refresh = () =>
    recordId &&
    listNotes(recordId)
      .then(setNotes)
      .catch((e) => pushToast(String(e), "error"));

  const onGenerate = async () => {
    if (!recordId) return;
    setStreamingNote("");
    try {
      // Regenerate once a note exists; both create a new retained active version.
      const id = await (active ? regenerateNote : generateNote)(recordId);
      if (id) await refresh(); // a cancelled run resolves null — keep the prior note
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  const onCancel = () => cancelGeneration().catch((e) => pushToast(String(e), "error"));

  const onRevert = async (noteId: string) => {
    if (!recordId) return;
    try {
      await revertVersion(recordId, noteId);
      await refresh();
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  return (
    <section aria-label="SOAP note" className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-neutral-200">SOAP note</h3>
        {generating ? (
          <button
            type="button"
            onClick={onCancel}
            className="rounded-md bg-red-600 px-3 py-1.5 text-sm font-medium hover:bg-red-500"
          >
            Cancel
          </button>
        ) : (
          <button
            type="button"
            onClick={onGenerate}
            disabled={!ready}
            className="rounded-md bg-teal-600 px-3 py-1.5 text-sm font-medium hover:bg-teal-500 disabled:opacity-40"
          >
            {active ? "Regenerate" : "Generate note"}
          </button>
        )}
      </div>

      {generating ? (
        <pre
          aria-label="Streaming note"
          className="whitespace-pre-wrap rounded-md border border-neutral-800 bg-neutral-900 p-3 text-sm text-neutral-300"
        >
          {streamingNote || "Generating…"}
        </pre>
      ) : active ? (
        <>
          <SoapEditor note={active} />
          {notes.length > 1 && (
            <div className="flex flex-col gap-1">
              <span className="text-xs font-semibold uppercase tracking-wide text-neutral-500">
                Versions
              </span>
              <ul className="flex flex-col gap-1">
                {notes.map((n) => (
                  <li key={n.id} className="flex items-center justify-between text-sm">
                    <span className="text-neutral-400">
                      {new Date(n.created_at * 1000).toLocaleString()}
                      {n.is_active && <span className="ml-2 text-teal-400">active</span>}
                    </span>
                    {!n.is_active && (
                      <button
                        type="button"
                        onClick={() => onRevert(n.id)}
                        className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800"
                      >
                        Revert
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      ) : (
        <p className="text-sm text-neutral-500">
          {recordId
            ? "No note yet — generate one from the transcript above."
            : "Stop a recording to generate a SOAP note."}
        </p>
      )}
    </section>
  );
}
