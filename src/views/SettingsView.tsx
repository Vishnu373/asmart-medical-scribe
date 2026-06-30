/** Settings view shell. Model/mic/hotkey/residency forms land in F6. */
export default function SettingsView() {
  return (
    <section aria-label="Settings" className="flex flex-1 flex-col p-6">
      <h2 className="text-lg font-semibold text-neutral-100">Settings</h2>
      <p className="mt-2 text-sm text-neutral-500">Model, microphone and hotkey settings (F6).</p>
    </section>
  );
}
