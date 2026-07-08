import { cancelGeneration, updateTranscript } from "@/bridge";
import { useAppStore } from "@/state";

/**
 * Post-ASR correction review (design §6.7). After Stop, the backend streams
 * `correction-suggestion` records — likely mishearings the deterministic word-fixer
 * can't catch — into a list beside the transcript. Each is **suggest-only**: the
 * transcript changes only when the clinician **accepts**, which replaces the flagged
 * span in place and rides the existing debounced autosave (§6.5). Reject dismisses.
 *
 * The pass is strictly additive: while it runs the panel shows a scanning hint and a
 * Cancel (the shared generation-cancel path); with no suggestions it renders nothing
 * and Generate simply becomes available.
 */
export default function CorrectionSuggestions() {
  const recordingState = useAppStore((s) => s.recordingState);
  const suggestions = useAppStore((s) => s.suggestions);
  const transcript = useAppStore((s) => s.transcript);
  const setTranscript = useAppStore((s) => s.setTranscript);
  const recordId = useAppStore((s) => s.currentRecordId);
  const removeSuggestion = useAppStore((s) => s.removeSuggestion);
  const pushToast = useAppStore((s) => s.pushToast);

  const correcting = recordingState === "CORRECTING";

  // Nothing to show and nothing running: stay out of the layout entirely so the
  // transcript keeps the full width (no suggestions → no panel, §6.7).
  if (!correcting && suggestions.length === 0) return null;

  // Accept: replace the first occurrence of the flagged span with the replacement
  // (duplicate spans → first not-yet-accepted occurrence, §6.7), then autosave. If
  // the span is no longer present (manually edited away), just dismiss it.
  const onAccept = (index: number) => {
    const { original, replacement } = suggestions[index];
    const next = transcript.replace(original, replacement);
    removeSuggestion(index);
    if (next === transcript) return;
    setTranscript(next);
    if (recordId) updateTranscript(recordId, next).catch((e) => pushToast(String(e), "error"));
  };

  const onCancel = () => cancelGeneration().catch((e) => pushToast(String(e), "error"));

  return (
    <aside aria-label="Suggested corrections" className="flex w-64 shrink-0 flex-col gap-2">
      <div className="flex items-center justify-between">
        <span className="text-xs font-semibold uppercase tracking-wide text-neutral-500">
          Suggested corrections{suggestions.length > 0 && ` (${suggestions.length})`}
        </span>
        {correcting && (
          <button
            type="button"
            onClick={onCancel}
            className="rounded border border-neutral-700 px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800"
          >
            Cancel
          </button>
        )}
      </div>

      {suggestions.length === 0 ? (
        <p className="text-xs text-neutral-600">
          {correcting ? "Scanning the transcript…" : null}
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {suggestions.map((s, i) => (
            <li
              key={`${s.original}→${s.replacement}-${i}`}
              className="flex flex-col gap-1.5 rounded-md border border-neutral-800 bg-neutral-900 p-2"
            >
              <div className="text-xs leading-snug">
                <span className="text-neutral-500 line-through">{s.original}</span>
                <span className="mx-1 text-neutral-600">→</span>
                <span className="text-teal-300">{s.replacement}</span>
              </div>
              <div className="flex gap-1.5">
                <button
                  type="button"
                  onClick={() => onAccept(i)}
                  className="rounded bg-teal-600 px-2 py-0.5 text-xs font-medium hover:bg-teal-500"
                >
                  Accept
                </button>
                <button
                  type="button"
                  onClick={() => removeSuggestion(i)}
                  className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800"
                >
                  Reject
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
