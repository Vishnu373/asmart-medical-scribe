import { useEffect, useRef, useState } from "react";
import { updateNote, type Note } from "@/bridge";
import { useAppStore } from "@/state";
import { parseSoap, serializeSoap, SOAP_ORDER, type SoapSections } from "@/lib/soap";

const SAVE_DEBOUNCE_MS = 600;

const LABEL: { [K in (typeof SOAP_ORDER)[number]]: string } = {
  subjective: "Subjective",
  objective: "Objective",
  assessment: "Assessment",
  plan: "Plan",
};

/**
 * Four-section editor for the active SOAP note (§8.5). Edits are debounce-saved
 * via `update_note` with the sections reassembled into headered markdown; a
 * pending save is flushed on unmount or when the note switches (regenerate /
 * revert) so the last edit is never lost.
 */
export default function SoapEditor({ note }: { note: Note }) {
  const pushToast = useAppStore((s) => s.pushToast);
  const [sections, setSections] = useState<SoapSections>(() => parseSoap(note.soap_data));

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<{ id: string; data: string } | null>(null);

  const flush = () => {
    if (timer.current) clearTimeout(timer.current);
    if (pending.current) {
      const { id, data } = pending.current;
      pending.current = null;
      updateNote(id, data).catch((e) => pushToast(String(e), "error"));
    }
  };

  // When the active note changes (regenerate/revert), flush the old edit then
  // reload the editor from the new note.
  useEffect(() => {
    flush();
    setSections(parseSoap(note.soap_data));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [note.id]);

  // Flush on unmount (e.g. switching views) rather than dropping the timer.
  useEffect(() => flush, []);

  const onEdit = (key: keyof SoapSections, value: string) => {
    const next = { ...sections, [key]: value };
    setSections(next);
    pending.current = { id: note.id, data: serializeSoap(next) };
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
  };

  return (
    <div className="flex flex-col gap-4">
      {SOAP_ORDER.map((key) => (
        <label key={key} className="flex flex-col gap-1">
          <span className="text-xs font-semibold uppercase tracking-wide text-neutral-400">
            {LABEL[key]}
          </span>
          <textarea
            aria-label={LABEL[key]}
            value={sections[key]}
            onChange={(e) => onEdit(key, e.target.value)}
            rows={3}
            className="resize-none rounded-md border border-neutral-800 bg-neutral-900 p-2 text-sm leading-relaxed text-neutral-100 focus:border-neutral-600 focus:outline-none"
          />
        </label>
      ))}
    </div>
  );
}
