/**
 * Header "Report a problem" control (§10.3): the "broke but didn't crash" channel
 * that complements crash reporting. Clicking opens a small form; the doctor types
 * what went wrong and submits, and the message flows through the telemetry seam
 * (`submit_feedback`) to the same backend as crashes.
 *
 * PHI caveat: the body is free text, so it can't be scrubbed — the form carries a
 * disclaimer asking the clinician not to include patient information.
 */
import { useState } from "react";
import { submitFeedback } from "@/bridge";
import { useAppStore } from "@/state";

export default function FeedbackButton() {
  const pushToast = useAppStore((s) => s.pushToast);
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const base =
    "rounded-md px-3 py-1.5 text-xs font-medium transition-colors disabled:cursor-not-allowed";

  function close() {
    setOpen(false);
    setMessage("");
  }

  async function submit() {
    const text = message.trim();
    if (!text || submitting) return;
    setSubmitting(true);
    try {
      await submitFeedback(text);
      pushToast("Thanks — your report was sent.", "info");
      close();
    } catch (err) {
      pushToast(`Couldn't send report: ${String(err)}`, "error");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className={`${base} bg-neutral-800 text-neutral-300 hover:bg-neutral-700`}
        title="Report a problem"
      >
        Report a problem
      </button>

      {open && (
        <div
          role="dialog"
          aria-label="Report a problem"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
        >
          <div className="w-full max-w-md rounded-lg border border-neutral-800 bg-neutral-900 p-4 shadow-xl">
            <h2 className="text-sm font-semibold text-neutral-100">Report a problem</h2>
            <p className="mt-1 text-xs text-neutral-400">
              Describe what went wrong. Please don&rsquo;t include patient information.
            </p>
            <textarea
              autoFocus
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              rows={5}
              placeholder="What happened?"
              className="mt-3 w-full resize-none rounded-md border border-neutral-700 bg-neutral-950 p-2 text-sm text-neutral-100 placeholder:text-neutral-600 focus:border-teal-600 focus:outline-none"
            />
            <div className="mt-3 flex justify-end gap-2">
              <button
                type="button"
                onClick={close}
                disabled={submitting}
                className={`${base} bg-neutral-800 text-neutral-300 hover:bg-neutral-700`}
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={submit}
                disabled={submitting || message.trim() === ""}
                className={`${base} bg-teal-600 text-white hover:bg-teal-500 disabled:opacity-50`}
              >
                {submitting ? "Sending…" : "Submit"}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
