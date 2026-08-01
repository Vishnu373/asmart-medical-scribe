import { useEffect, useRef, useState } from "react";
import {
  downloadLlm,
  downloadStt,
  frontendReady,
  getLlmStatus,
  markSetupCompleted,
  onLlmStatus,
  onModelDownloadDone,
  onModelDownloadError,
  onModelDownloadProgress,
  setupStatus,
} from "@/bridge";
import type { SetupStatus } from "@/bridge";

/** The note model's download event key (matches `models::LLM.tier`). */
const LLM = "llm";
/** The Parakeet STT download's event key (matches `models::STT.tier`). */
const STT = "stt";

/** A required model the first run must fetch, derived from `SetupStatus`. */
interface Slot {
  /** Event key: `"llm"` or `"stt"`. */
  key: string;
  label: string;
  present: boolean;
}

function slotsFor(s: SetupStatus): Slot[] {
  return [
    { key: LLM, label: "Note model", present: s.llm_present },
    { key: STT, label: "Speech recognition", present: s.stt_present },
  ];
}

/**
 * First-run Setup gate (§8.2, D3). The installer ships no model weights, so on
 * first launch the required set — the RAM-fit LLM and the Parakeet STT model — is
 * downloaded once, verified, and cached. Missing models start downloading
 * automatically; a failed one shows a Retry.
 *
 * The downloads are not the last step: the note model's first load primes the
 * prompt-prefix KV cache (§8.7), ~22s that would otherwise land on an apparently
 * stalled main screen. So Setup holds for a third step, "Preparing note model…",
 * until the model reports ready. The app is blocked behind this screen until all
 * three finish; thereafter Setup is skipped entirely.
 */
export default function SetupView({ onReady }: { onReady: () => void }) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  // Third step: downloads are verified and we are waiting on the note model's load.
  const [priming, setPriming] = useState(false);
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
    const call = key === STT ? downloadStt() : downloadLlm();
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
            if (s.ready) {
              // Reached here only via a completed download → a genuine first-run
              // setup finish (§3 telemetry). Best-effort; never block the UI.
              void markSetupCompleted().catch(() => {});
              // onReady();
              // Not done yet — hand over to the priming step below.
              setPriming(true);
            }
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
  }, []);

  // Third step: load + prime the note model (§8.2 preload, §8.7 prefix cache) and
  // hold the screen until it reports in. Usually the app's own `frontend_ready` at
  // mount fired before the weights existed and failed, which re-arms the backend
  // gate, so this is the call that actually loads them.
  useEffect(() => {
    if (!priming) return;
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      onReady();
    };
    const unlisten = onLlmStatus((p) => {
      // On `error`, release anyway rather than trapping the doctor on Setup — the
      // main screen already surfaces a failed note model.
      if (p.status === "ready" || p.status === "error") finish();
    }).then((fn) => {
      // Started only once the listener is attached, so a fast load can't land first.
      void frontendReady().catch(finish);
      // The gate is one-shot on success: if the mount-time preload already loaded the
      // model (weights present, only STT was missing), no `llm-status` is coming.
      void getLlmStatus()
        .then((s) => {
          if (s === "ready") finish();
        })
        .catch(finish);
      return fn;
    });
    return () => {
      unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [priming]);

  const slots = status ? slotsFor(status) : [];

  return (
    <div className="flex h-screen flex-col items-center justify-center bg-neutral-950 p-8 text-neutral-100">
      <div className="w-full max-w-md">
        <h1 className="text-lg font-semibold">Setting up ASmart Medical Scribe</h1>
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
          {priming && (
            <li className="flex flex-col gap-1">
              <div className="flex items-center justify-between text-sm">
                <span className="font-medium text-neutral-200">Preparing note model…</span>
                <span className="text-neutral-400">One moment</span>
              </div>
              {/* No percentage: the backend reports only loading → ready, so an
                  indeterminate bar is the honest rendering of a ~22s prime. */}
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-neutral-800">
                <div className="h-full w-1/3 animate-pulse rounded-full bg-teal-500" />
              </div>
            </li>
          )}
        </ul>
      </div>
    </div>
  );
}
