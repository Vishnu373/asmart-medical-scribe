import { useEffect, useState } from "react";
import {
  downloadModel,
  getSettings,
  listInputDevices,
  modelStatus,
  onModelDownloadDone,
  onModelDownloadError,
  onModelDownloadProgress,
  updateSettings,
} from "@/bridge";
import type { InputDevice, ModelStatus, Settings } from "@/bridge";
import { useAppStore } from "@/state";

/** Doctor-facing model tiers (§9.3 `model_choice`). */
const MODELS: { value: string; label: string }[] = [
  { value: "best", label: "Best — Mistral-7B (most accurate, needs the most RAM)" },
  { value: "medium", label: "Medium — Phi-3.5 Q8" },
  { value: "okay", label: "Okay — Phi-3.5 Q4 (lightest)" },
];

/** Manual residency force (§7). `null` = use the automatic per-machine decision. */
const RESIDENCY: { value: string; label: string }[] = [
  { value: "", label: "Automatic (recommended)" },
  { value: "co_resident", label: "Keep both models resident" },
  { value: "swap", label: "Swap models (lower RAM)" },
];

/**
 * Settings view (§9.3, F6). The doctor-facing keys are model, microphone, and the
 * residency override. (The paste-hotkey control is removed while EMR hand-off is
 * manual copy/paste, F7; the persisted value is untouched for when the hotkey
 * returns.) Internal keys (`residency_mode`, `observed_total_ram`, VAD, idle
 * timeout) are never shown and are preserved across save by spreading the loaded
 * object (read-modify-write).
 */
export default function SettingsView() {
  const settings = useAppStore((s) => s.settings);
  const setSettings = useAppStore((s) => s.setSettings);
  const pushToast = useAppStore((s) => s.pushToast);

  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [models, setModels] = useState<ModelStatus[]>([]);
  // Download progress for the optional tier, 0–100; null when not downloading.
  const [downloadPct, setDownloadPct] = useState<number | null>(null);
  // Local edit buffer; null until the initial load resolves.
  const [form, setForm] = useState<Settings | null>(settings);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    getSettings()
      .then((s) => {
        setSettings(s);
        setForm(s);
      })
      .catch((e) => pushToast(String(e), "error"));
    listInputDevices()
      .then(setDevices)
      .catch((e) => pushToast(String(e), "error"));
    modelStatus()
      .then(setModels)
      .catch((e) => pushToast(String(e), "error"));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Subscribe to download progress/result events for the optional model (D1).
  useEffect(() => {
    const unlisten = Promise.all([
      onModelDownloadProgress((p) => {
        setDownloadPct(p.total > 0 ? Math.round((p.downloaded / p.total) * 100) : 0);
      }),
      onModelDownloadDone(() => {
        setDownloadPct(null);
        modelStatus().then(setModels).catch(() => {});
        pushToast("Model downloaded.", "info");
      }),
      onModelDownloadError((e) => {
        setDownloadPct(null);
        pushToast(e.message, "error");
      }),
    ]);
    return () => {
      unlisten.then((fns) => fns.forEach((fn) => fn()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const okay = models.find((m) => m.tier === "okay");
  const okayNeedsDownload = okay?.optional && !okay.present;

  const onDownloadOkay = () => {
    setDownloadPct(0);
    downloadModel("okay").catch((e) => {
      setDownloadPct(null);
      pushToast(String(e), "error");
    });
  };

  if (!form) {
    return (
      <section aria-label="Settings" className="flex flex-1 flex-col p-6">
        <h2 className="text-lg font-semibold text-neutral-100">Settings</h2>
        <p className="mt-2 text-sm text-neutral-500">Loading…</p>
      </section>
    );
  }

  const patch = (next: Partial<Settings>) => {
    setForm({ ...form, ...next });
    setSaved(false);
  };

  const onSave = async () => {
    try {
      await updateSettings(form);
      setSettings(form);
      setSaved(true);
    } catch (e) {
      pushToast(String(e), "error");
    }
  };

  return (
    <section aria-label="Settings" className="flex flex-1 flex-col gap-6 p-6">
      <h2 className="text-lg font-semibold text-neutral-100">Settings</h2>

      <label className="flex flex-col gap-1">
        <span className="text-sm font-medium text-neutral-200">Note model</span>
        <select
          aria-label="Note model"
          value={form.model_choice}
          onChange={(e) => patch({ model_choice: e.target.value })}
          className="rounded-md border border-neutral-800 bg-neutral-900 px-3 py-2 text-sm text-neutral-100 focus:border-neutral-600 focus:outline-none"
        >
          {MODELS.map((m) => {
            // The optional "okay" tier can't be selected until it's downloaded.
            const needsDownload = m.value === "okay" && okayNeedsDownload;
            return (
              <option key={m.value} value={m.value} disabled={needsDownload}>
                {m.label}
                {needsDownload ? " — download required" : ""}
              </option>
            );
          })}
        </select>
        {okayNeedsDownload && (
          <div className="mt-1 flex items-center gap-3 text-sm text-neutral-400">
            {downloadPct === null ? (
              <>
                <span>The “Okay” model isn’t installed.</span>
                <button
                  type="button"
                  onClick={onDownloadOkay}
                  className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800"
                >
                  Download
                </button>
              </>
            ) : (
              <span>Downloading… {downloadPct}%</span>
            )}
          </div>
        )}
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-sm font-medium text-neutral-200">Microphone</span>
        <select
          aria-label="Microphone"
          value={form.mic_device ?? ""}
          onChange={(e) => patch({ mic_device: e.target.value || null })}
          className="rounded-md border border-neutral-800 bg-neutral-900 px-3 py-2 text-sm text-neutral-100 focus:border-neutral-600 focus:outline-none"
        >
          <option value="">System default</option>
          {devices.map((d) => (
            <option key={d.name} value={d.name}>
              {d.name}
              {d.is_default ? " (default)" : ""}
            </option>
          ))}
        </select>
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-sm font-medium text-neutral-200">Model residency</span>
        <select
          aria-label="Model residency"
          value={form.residency_override ?? ""}
          onChange={(e) => patch({ residency_override: e.target.value || null })}
          className="rounded-md border border-neutral-800 bg-neutral-900 px-3 py-2 text-sm text-neutral-100 focus:border-neutral-600 focus:outline-none"
        >
          {RESIDENCY.map((r) => (
            <option key={r.value} value={r.value}>
              {r.label}
            </option>
          ))}
        </select>
      </label>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onSave}
          className="rounded-md bg-teal-600 px-4 py-2 text-sm font-medium hover:bg-teal-500"
        >
          Save
        </button>
        {saved && <span className="text-sm text-teal-400">Saved.</span>}
      </div>
    </section>
  );
}
