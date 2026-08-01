import { useEffect, useState } from "react";
import { frontendReady, getLlmStatus, ping, setupStatus } from "@/bridge";
import { useAppStore } from "@/state";
import { useBackendEvents } from "@/hooks/useBackendEvents";
import { useUpdateCheck } from "@/hooks/useUpdateCheck";
import NavBar from "@/components/NavBar";
import Toaster from "@/components/Toaster";
import UpdateButton from "@/components/UpdateButton";
import FeedbackButton from "@/components/FeedbackButton";
import RecordingView from "@/views/RecordingView";
import RecordsView from "@/views/RecordsView";
import SettingsView from "@/views/SettingsView";
import SetupView from "@/views/SetupView";
// import ExpiredView from "@/views/ExpiredView";

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
  // Trial gate removed: the app no longer hard-stops on a compiled-in end date.
  // const [trial, setTrial] = useState<{ expired: boolean; endDate: string } | null>(null);
  // First-run gate (D3): `null` while checking, `false` until the required models
  // are downloaded (show Setup), `true` once the app can run.
  const [setupReady, setSetupReady] = useState<boolean | null>(null);
  // Note-model readiness (§8.2 startup fix): seeded here at mount; surfaced in the
  // recording-view StatusBadge (Idle → Loading → Ready).
  const seedLlmStatus = useAppStore((s) => s.seedLlmStatus);

  // Wire backend → UI events (§9.5) into the store, once, at the root.
  useBackendEvents();

  // On startup, check for and apply an app update (separate binary-only channel).
  useUpdateCheck();

  // Liveness probe through the typed bridge (carried over from B1).
  useEffect(() => {
    ping("ready")
      .then(() => setBridgeOk(true))
      .catch(() => setBridgeOk(false));
  }, []);

  // Tell the backend the UI has mounted so the co-resident note-model warm starts
  // *after* the window has painted, not during launch — warming inside `setup`
  // ghosted the window as "not responding" (§8.2 startup fix). Fire-and-forget;
  // idempotent on the backend.
  useEffect(() => {
    void frontendReady().catch(() => {});
  }, []);

  // useEffect(() => {
  //   trialStatus()
  //     .then((s) => setTrial({ expired: s.expired, endDate: s.end_date }))
  //     .catch(() => setTrial({ expired: false, endDate: "" }));
  // }, []);

  // Are the required models present? If not, Setup downloads them first (D3).
  useEffect(() => {
    setupStatus()
      .then((s) => setSetupReady(s.ready))
      .catch(() => setSetupReady(true)); // don't hard-block if the check fails
  }, []);

  // Seed the note-model status before the async `llm-status` event arrives — the
  // co-resident preload emits `loading` before the webview has a listener, so this
  // mount query is how the UI learns the model is still warming (§8.2 startup fix).
  useEffect(() => {
    getLlmStatus()
      .then(seedLlmStatus)
      // The default is the safe "loading"; if the command is somehow unavailable,
      // clear the hint to "ready" rather than stranding Generate disabled.
      .catch(() => seedLlmStatus("ready"));
  }, [seedLlmStatus]);

  // if (trial === null || setupReady === null) {
  if (setupReady === null) {
    return <div className="h-screen bg-neutral-950" aria-label="loading" />;
  }
  // if (trial.expired) {
  //   return <ExpiredView endDate={trial.endDate} />;
  // }
  if (!setupReady) {
    return <SetupView onReady={() => setSetupReady(true)} />;
  }

  return (
    <div className="flex h-screen flex-col bg-neutral-950 text-neutral-100">
      <header className="flex items-center justify-between border-b border-neutral-800 px-4 py-3">
        <h1 className="text-base font-semibold">ASmart Medical Scribe</h1>
        <div className="flex items-center gap-3">
          <UpdateButton />
          <FeedbackButton />
          <span
            aria-label="bridge status"
            className={`h-2 w-2 rounded-full ${
              bridgeOk === null ? "bg-neutral-600" : bridgeOk ? "bg-teal-400" : "bg-red-500"
            }`}
          />
        </div>
      </header>
      <div className="flex flex-1 overflow-hidden">
        <NavBar />
        <main className="flex-1 overflow-y-auto">
          <ActiveView />
        </main>
      </div>
      <Toaster />
    </div>
  );
}

export default App;
