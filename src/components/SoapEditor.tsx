import { useEffect, useRef, useState } from "react";
import { copyToClipboard, updateNote, type Note } from "@/bridge";
import { useAppStore } from "@/state";
import { toPlainText } from "@/lib/soap";
import { useAutoGrow } from "@/hooks/useAutoGrow";
import Markdown from "@/components/Markdown";

const SAVE_DEBOUNCE_MS = 600;

type Mode = "preview" | "edit";

/**
 * Single-window editor for the active SOAP note (§8.5). The note is one markdown
 * document with an Obsidian-style Edit ⇄ Preview toggle: Preview renders it as
 * formatted, read-only HTML (the default — clinicians read far more than they
 * edit); Edit drops back to the raw textarea, debounce-saved verbatim via
 * `update_note`. A pending save is flushed on unmount or when the note switches
 * (regenerate / revert) so the last edit is never lost. `busy` holds Copy off
 * while a regenerate/revert is in flight, when `note` is still the old version.
 */
export default function SoapEditor({ note, busy }: { note: Note; busy?: boolean }) {
  const pushToast = useAppStore((s) => s.pushToast);
  const [text, setText] = useState(note.soap_data);
  const [mode, setMode] = useState<Mode>("preview");

  const ref = useRef<HTMLTextAreaElement>(null);
  // Grow with content so the note is never a squeezed scroll-box; the page scrolls.
  // `mode` is folded into the trigger so the textarea also re-grows the moment it
  // mounts (entering Edit) rather than only on the next keystroke.
  useAutoGrow(ref, `${mode}:${text}`);

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<{ id: string; data: string } | null>(null);

  // Resolves false when the save failed, so `onCopy` can await it and hold the
  // clipboard back; fire-and-forget only guaranteed the write was *started*.
  const flush = async () => {
    if (timer.current) clearTimeout(timer.current);
    if (!pending.current) return true;
    const { id, data } = pending.current;
    pending.current = null;
    try {
      await updateNote(id, data);
      return true;
    } catch (e) {
      pushToast(String(e), "error");
      return false;
    }
  };

  // When the active note changes (regenerate/revert), flush the old edit then
  // reload the editor from the new note.
  useEffect(() => {
    flush();
    setText(note.soap_data);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [note.id]);

  // Flush on unmount (e.g. switching views) rather than dropping the timer.
  // `flush` is intentionally not a dep: it is rebuilt every render, so depending on
  // it would tear down and re-run this effect — flushing constantly, not on unmount.
  // `void`: the component is going away, so there is no one left to await the save.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => () => void flush(), []);

  const onEdit = (value: string) => {
    setText(value);
    pending.current = { id: note.id, data: value };
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
  };

  // Manual EMR hand-off (F7): copy the whole note so the clinician can paste it
  // into the EMR with Ctrl+V. Copies the live editor value rendered to plain
  // text — what Preview shows, minus the markdown. Flush first so a copy made
  // inside the debounce window can't hand over text the DB doesn't have yet.
  const onCopy = async () => {
    // A failed save already toasted; copying anyway would put text on the
    // clipboard that the record does not have.
    if (!(await flush())) return;
    const out = toPlainText(text);
    if (!out) return;
    copyToClipboard(out)
      .then(() => pushToast("Note copied", "info"))
      .catch((e) => pushToast(String(e), "error"));
  };

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <div
          role="tablist"
          aria-label="Note view"
          className="flex rounded-md border border-neutral-800 p-0.5 text-xs"
        >
          {(["preview", "edit"] as const).map((m) => (
            <button
              key={m}
              type="button"
              role="tab"
              aria-selected={mode === m}
              onClick={() => setMode(m)}
              className={`rounded px-2 py-0.5 capitalize ${
                mode === m ? "bg-neutral-700 text-neutral-100" : "text-neutral-400 hover:text-neutral-200"
              }`}
            >
              {m}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={onCopy}
          disabled={busy || !text.trim()}
          className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-40"
        >
          Copy
        </button>
      </div>
      {mode === "preview" ? (
        <div
          aria-label="SOAP note preview"
          className="min-h-64 rounded-md border border-neutral-800 bg-neutral-900 p-3"
        >
          {text.trim() ? (
            <Markdown>{text}</Markdown>
          ) : (
            <p className="text-sm text-neutral-500">Empty note.</p>
          )}
        </div>
      ) : (
        <textarea
          ref={ref}
          aria-label="SOAP note"
          value={text}
          onChange={(e) => onEdit(e.target.value)}
          className="min-h-64 resize-none overflow-hidden rounded-md border border-neutral-800 bg-neutral-900 p-3 text-sm leading-relaxed text-neutral-100 focus:border-neutral-600 focus:outline-none"
        />
      )}
    </div>
  );
}
