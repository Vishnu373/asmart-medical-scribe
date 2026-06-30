import { useEffect, useState } from "react";
import { ping } from "@/bridge";
import { useAppStore } from "@/state";
import NavBar from "@/components/NavBar";
import RecordingView from "@/views/RecordingView";
import RecordsView from "@/views/RecordsView";
import SettingsView from "@/views/SettingsView";

function ActiveView() {
  const view = useAppStore((s) => s.view);
  switch (view) {
    case "recording":
      return <RecordingView />;
    case "records":
      return <RecordsView />;
    case "settings":
      return <SettingsView />;
  }
}

function App() {
  const [bridgeOk, setBridgeOk] = useState<boolean | null>(null);

  // Liveness probe through the typed bridge (carried over from B1).
  useEffect(() => {
    ping("ready")
      .then(() => setBridgeOk(true))
      .catch(() => setBridgeOk(false));
  }, []);

  return (
    <div className="flex h-screen flex-col bg-neutral-950 text-neutral-100">
      <header className="flex items-center justify-between border-b border-neutral-800 px-4 py-3">
        <h1 className="text-base font-semibold">Medical Scribe</h1>
        <span
          aria-label="bridge status"
          className={`h-2 w-2 rounded-full ${
            bridgeOk === null ? "bg-neutral-600" : bridgeOk ? "bg-teal-400" : "bg-red-500"
          }`}
        />
      </header>
      <div className="flex flex-1 overflow-hidden">
        <NavBar />
        <main className="flex flex-1 overflow-auto">
          <ActiveView />
        </main>
      </div>
    </div>
  );
}

export default App;
