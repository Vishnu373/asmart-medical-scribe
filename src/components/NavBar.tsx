/** Left-rail navigation between the three top-level views. */
import { useAppStore, type View } from "@/state";

const ITEMS: { view: View; label: string }[] = [
  { view: "recording", label: "Recording" },
  { view: "records", label: "Records" },
  { view: "settings", label: "Settings" },
];

export default function NavBar() {
  const view = useAppStore((s) => s.view);
  const setView = useAppStore((s) => s.setView);
  const recordingState = useAppStore((s) => s.recordingState);

  // While a session is live (RECORDING/PROCESSING/GENERATING), leaving the
  // Recording view would point the editors at another record while the backend
  // is still streaming into the current one — corrupting state. Lock navigation
  // to other views until the app is back to IDLE.
  const busy = recordingState !== "IDLE";

  return (
    <nav aria-label="Primary" className="flex w-44 flex-col gap-1 border-r border-neutral-800 p-3">
      {ITEMS.map((item) => {
        const active = view === item.view;
        const locked = busy && !active;
        return (
          <button
            key={item.view}
            type="button"
            aria-current={active ? "page" : undefined}
            disabled={locked}
            title={locked ? "Finish the current recording first" : undefined}
            onClick={() => setView(item.view)}
            className={`rounded-md px-3 py-2 text-left text-sm transition-colors disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent ${
              active
                ? "bg-teal-600/20 text-teal-300"
                : "text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200"
            }`}
          >
            {item.label}
          </button>
        );
      })}
    </nav>
  );
}
