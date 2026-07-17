import { useEffect, useState } from "react";
import { deleteRecord, listNotes, listRecords, openRecord } from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Records browser (FR-13). Lists saved encounters from `list_records`; opening
 * one loads its transcript + note versions into the shared store and switches to
 * the Recording view, whose transcript/SOAP editors are keyed off the active
 * record id. Delete is permanent (NFR-9) so it takes an inline confirm.
 */
export default function RecordsView() {
  const records = useAppStore((s) => s.records);
  const setRecords = useAppStore((s) => s.setRecords);
  const setView = useAppStore((s) => s.setView);
  const setCurrentRecordId = useAppStore((s) => s.setCurrentRecordId);
  const setTranscript = useAppStore((s) => s.setTranscript);
  const setSegments = useAppStore((s) => s.setSegments);
  const setNotes = useAppStore((s) => s.setNotes);
  const setStreamingNote = useAppStore((s) => s.setStreamingNote);
  const pushToast = useAppStore((s) => s.pushToast);

  // The record id armed for deletion, if any — one at a time.
  const [confirming, setConfirming] = useState<string | null>(null);

  const refresh = () =>
    listRecords()
      .then(setRecords)
      .catch((e) => pushToast(String(e), "error"));

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onOpen = async (id: string) => {
    try {
      const record = await openRecord(id);
      if (!record) return; // deleted out from under us
      const notes = await listNotes(id);
      setCurrentRecordId(record.id);
      setSegments([]); // a loaded record edits its saved transcript, no live stream
      setTranscript(record.transcript);
      setStreamingNote("");
      setNotes(notes);
      setView("recording");
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  const onDelete = async (id: string) => {
    try {
      await deleteRecord(id);
      setConfirming(null);
      await refresh();
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  return (
    <section aria-label="Records" className="flex flex-1 flex-col gap-4 p-6">
      <h2 className="text-lg font-semibold text-neutral-100">Records</h2>

      {records.length === 0 ? (
        <p className="text-sm text-neutral-500">No saved encounters yet.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {records.map((r) => (
            <li
              key={r.id}
              className="flex items-center justify-between gap-4 rounded-md border border-neutral-800 bg-neutral-900 px-4 py-3"
            >
              <div className="min-w-0">
                <p className="truncate text-sm font-medium text-neutral-100">{r.label}</p>
                <p className="text-xs text-neutral-500">
                  {new Date(r.created_at * 1000).toLocaleString()}
                  <span className="ml-2 uppercase">{r.language}</span>
                </p>
              </div>

              <div className="flex shrink-0 items-center gap-2">
                {confirming === r.id ? (
                  <>
                    <span className="text-xs text-neutral-400">Delete permanently?</span>
                    <button
                      type="button"
                      onClick={() => onDelete(r.id)}
                      className="rounded-md bg-red-600 px-3 py-1.5 text-sm font-medium hover:bg-red-500"
                    >
                      Confirm
                    </button>
                    <button
                      type="button"
                      onClick={() => setConfirming(null)}
                      className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800"
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      type="button"
                      onClick={() => onOpen(r.id)}
                      className="rounded-md bg-teal-600 px-3 py-1.5 text-sm font-medium hover:bg-teal-500"
                    >
                      Open
                    </button>
                    <button
                      type="button"
                      onClick={() => setConfirming(r.id)}
                      className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800"
                    >
                      Delete
                    </button>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
