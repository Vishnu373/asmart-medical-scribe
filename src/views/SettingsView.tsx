import { useEffect, useState } from "react";
import { getSettings, listInputDevices, rebindPasteHotkey, updateSettings } from "@/bridge";
import type { InputDevice, Settings } from "@/bridge";
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

const MODIFIERS = new Set(["Control", "Alt", "Shift", "Meta"]);

/**
 * Settings view (§9.3, F6). The doctor-facing keys are deliberately few — model,
 * microphone, paste hotkey — plus the residency override. Internal keys
 * (`residency_mode`, `observed_total_ram`, VAD, idle timeout) are never shown and
 * are preserved across save by spreading the loaded object (read-modify-write).
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

  // Capture a modifier+key combo (§9.3: rebindable 2-key hotkey). A lone key with
  // no modifier is rejected so the paste hotkey can't collide with normal typing.
  const onHotkeyKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    e.preventDefault();
    if (MODIFIERS.has(e.key)) return; // still holding only modifiers
    const mods: string[] = [];
    if (e.ctrlKey) mods.push("Ctrl");
    if (e.altKey) mods.push("Alt");
    if (e.shiftKey) mods.push("Shift");
    // Emit "Super" (not "Win") — the backend's accelerator parser only knows
    // ALT/CTRL/SHIFT/COMMAND/SUPER; an unrecognized "Win" token would be treated
    // as the main key and fail to register at launch.
    if (e.metaKey) mods.push("Super");
    if (mods.length === 0) {
      pushToast("Hotkey must include a modifier (Ctrl, Alt, Shift or Win).", "info");
      return;
    }
    const key = e.key.length === 1 ? e.key.toUpperCase() : e.key;
    patch({ paste_hotkey: [...mods, key].join("+") });
  };

  const onSave = async () => {
    try {
      await updateSettings(form);
      setSettings(form);
      // Re-register the global hotkey so a rebind applies live (§8.6) instead of
      // only after the next launch. A failure here (combo already taken) is
      // surfaced but doesn't undo the saved settings.
      await rebindPasteHotkey(form.paste_hotkey);
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
          {MODELS.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </select>
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
        <span className="text-sm font-medium text-neutral-200">Paste hotkey</span>
        <input
          aria-label="Paste hotkey"
          readOnly
          value={form.paste_hotkey}
          onKeyDown={onHotkeyKeyDown}
          placeholder="Focus and press a combo (e.g. Alt+P)"
          className="w-48 cursor-pointer rounded-md border border-neutral-800 bg-neutral-900 px-3 py-2 text-sm text-neutral-100 focus:border-neutral-600 focus:outline-none"
        />
        <span className="text-xs text-neutral-500">
          Used to paste a SOAP section into the focused EMR field.
        </span>
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
