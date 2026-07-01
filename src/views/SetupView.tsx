import { useEffect, useRef, useState } from "react";
import {
  downloadModel,
  downloadStt,
  onModelDownloadDone,
  onModelDownloadError,
  onModelDownloadProgress,
  setupStatus,
} from "@/bridge";
import type { SetupStatus } from "@/bridge";

/** The Parakeet STT download's event key (matches `models::STT.tier`). */
const STT = "stt";

/** A required model the first run must fetch, derived from `SetupStatus`. */
interface Slot {
  /** Event key: the LLM tier (`best`/`medium`) or `"stt"`. */
  key: string;
  label: string;
  present: boolean;
}

function slotsFor(s: SetupStatus): Slot[] {
  return [
    { key: s.llm_tier, label: "Note model", present: s.llm_present },
    { key: STT, label: "Speech recognition", present: s.stt_present },
  ];
}

/**
 * First-run Setup gate (§8.2, D3). The installer ships no model weights, so on
 * first launch the required set — the RAM-fit LLM and the Parakeet STT model — is
 * downloaded once, verified, and cached. The app is blocked behind this screen
 * until both are present; thereafter Setup is skipped entirely. Missing models
 * start downloading automatically; a failed one shows a Retry.
 */
export default function SetupView({ onReady }: { onReady: () => void }) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  // Percent per download key (absent ⇒ not started); error message per key.
  const [progress, setProgress] = useState<Record<string, number>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Keys already kicked off, so re-renders/re-fetches don't double-start a worker.
  const started = useRef<Set<string>>(new Set());

  const start = (key: string) => {
    started.current.add(key);
    setErrors((e) => {
      const next = { ...e };
      delete next[key];
      return next;
    });
    setProgress((p) => ({ ...p, [key]: 0 }));
    const call = key === STT ? downloadStt() : downloadModel(key);
    call.catch((err) => {
      started.current.delete(key);
      setErrors((e) => ({ ...e, [key]: String(err) }));
    });
  };

  // Load status once, then auto-start any missing download.
  useEffect(() => {
    setupStatus()
      .then((s) => {
        setStatus(s);
        if (s.ready) {
          onReady();
          return;
        }
        slotsFor(s)
          .filter((slot) => !slot.present && !started.current.has(slot.key))
          .forEach((slot) => start(slot.key));
      })
      .catch(() => onReady()); // an older backend without setup_status shouldn't hard-block
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Live download progress and terminal results.
  useEffect(() => {
    const unlisten = Promise.all([
      onModelDownloadProgress((p) => {
        const pct = p.total > 0 ? Math.round((p.downloaded / p.total) * 100) : 0;
        setProgress((prev) => ({ ...prev, [p.tier]: pct }));
      }),
      onModelDownloadDone((e) => {
        started.current.delete(e.tier);
        setProgress((prev) => ({ ...prev, [e.tier]: 100 }));
        setupStatus()
          .then((s) => {
            setStatus(s);
            if (s.ready) onReady();
          })
          .catch(() => {});
      }),
      onModelDownloadError((e) => {
        started.current.delete(e.tier);
        setProgress((prev) => {
          const next = { ...prev };
          delete next[e.tier];
          return next;
        });
        setErrors((prev) => ({ ...prev, [e.tier]: e.message }));
      }),
    ]);
    return () => {
      unlisten.then((fns) => fns.forEach((fn) => fn()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const slots = status ? slotsFor(status) : [];

  return (
    <div className="flex h-screen flex-col items-center justify-center bg-neutral-950 p-8 text-neutral-100">
      <div className="w-full max-w-md">
        <h1 className="text-lg font-semibold">Setting up Medical Scribe</h1>
        <p className="mt-2 text-sm text-neutral-400">
          Downloading the on-device models. This happens once — everything runs
          offline afterwards.
        </p>

        <ul className="mt-6 flex flex-col gap-4">
          {slots.map((slot) => {
            const pct = progress[slot.key];
            const err = errors[slot.key];
            const done = slot.present || pct === 100;
            return (
              <li key={slot.key} className="flex flex-col gap-1">
                <div className="flex items-center justify-between text-sm">
                  <span className="font-medium text-neutral-200">{slot.label}</span>
                  <span className="text-neutral-400">
                    {done
                      ? "Ready"
                      : err
                        ? "Failed"
                        : pct === undefined
                          ? "Starting…"
                          : `${pct}%`}
                  </span>
                </div>
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-neutral-800">
                  <div
                    className={`h-full rounded-full ${err ? "bg-red-500" : "bg-teal-500"}`}
                    style={{ width: done ? "100%" : `${pct ?? 0}%` }}
                  />
                </div>
                {err && (
                  <div className="flex items-center gap-2 text-xs text-red-400">
                    <span className="flex-1 truncate">{err}</span>
                    <button
                      type="button"
                      onClick={() => start(slot.key)}
                      className="rounded border border-neutral-700 px-2 py-0.5 text-neutral-200 hover:bg-neutral-800"
                    >
                      Retry
                    </button>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}
