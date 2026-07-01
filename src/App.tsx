import { useEffect, useState } from "react";
import { ping, setupStatus } from "@/bridge";
import { useAppStore } from "@/state";
import { useBackendEvents } from "@/hooks/useBackendEvents";
import NavBar from "@/components/NavBar";
import Toaster from "@/components/Toaster";
import RecordingView from "@/views/RecordingView";
import RecordsView from "@/views/RecordsView";
import SettingsView from "@/views/SettingsView";
import SetupView from "@/views/SetupView";

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
  // First-run gate (D3): `null` while checking, `false` until the required models
  // are downloaded (show Setup), `true` once the app can run.
  const [setupReady, setSetupReady] = useState<boolean | null>(null);

  // Wire backend → UI events (§9.5) into the store, once, at the root.
  useBackendEvents();

  // Liveness probe through the typed bridge (carried over from B1).
  useEffect(() => {
    ping("ready")
      .then(() => setBridgeOk(true))
      .catch(() => setBridgeOk(false));
  }, []);

  // Are the required models present? If not, Setup downloads them first (D3).
  useEffect(() => {
    setupStatus()
      .then((s) => setSetupReady(s.ready))
      .catch(() => setSetupReady(true)); // don't hard-block if the check fails
  }, []);

  if (setupReady === null) {
    return <div className="h-screen bg-neutral-950" aria-label="loading" />;
  }
  if (!setupReady) {
    return <SetupView onReady={() => setSetupReady(true)} />;
  }

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
      <Toaster />
    </div>
  );
}

export default App;
