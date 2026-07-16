import { useEffect, useState } from "react";
import { getSettings, listInputDevices, updateSettings } from "@/bridge";
import type { InputDevice, Settings } from "@/bridge";
import { useAppStore } from "@/state";

/** Manual residency force (§7). `null` = use the automatic per-machine decision. */
const RESIDENCY: { value: string; label: string }[] = [
  { value: "", label: "Automatic (recommended)" },
  { value: "co_resident", label: "Keep both models resident" },
  { value: "swap", label: "Swap models (lower RAM)" },
];

/**
 * Settings view (§9.3, F6). The doctor-facing keys are microphone and the residency
 * override. (There is one note model now — §3 single-model refactor — so the model
 * picker is gone. The paste-hotkey control is likewise absent while EMR hand-off is
 * manual copy/paste, F7.) Internal keys (`residency_mode`, `observed_total_ram`, VAD,
 * idle timeout) are never shown and are preserved across save by spreading the loaded
 * object (read-modify-write).
 */
export default function SettingsView() {
  const settings = useAppStore((s) => s.settings);
  const setSettings = useAppStore((s) => s.setSettings);
  const pushToast = useAppStore((s) => s.pushToast);

  const [devices, setDevices] = useState<InputDevice[]>([]);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
