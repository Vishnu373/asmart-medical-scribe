/**
 * Header app-update control: a single button that morphs through the update
 * state machine (§ updater). A passive background check ([`useUpdateCheck`])
 * flips the store to `available`; from there the doctor drives everything:
 *
 *   available   → "Update available"   click → download() in the background
 *   downloading → "Downloading… (n%)"  (disabled; app stays usable)
 *   ready       → "Install & restart"  click → install() + relaunch()
 *   installing  → "Installing…"        (disabled)
 *
 * The download runs in the background (non-blocking); the install — which
 * restarts the app — happens ONLY on an explicit click, never automatically.
 */
import { relaunch } from "@tauri-apps/plugin-process";
import { useAppStore } from "@/state";

export default function UpdateButton() {
  const update = useAppStore((s) => s.update);
  const stage = useAppStore((s) => s.updateStage);
  const progress = useAppStore((s) => s.updateProgress);
  const setStage = useAppStore((s) => s.setUpdateStage);
  const setProgress = useAppStore((s) => s.setUpdateProgress);
  const pushToast = useAppStore((s) => s.pushToast);

  // Hidden unless an update is somewhere in the flow.
  if (!update || stage === "idle") return null;

  async function startDownload() {
    if (!update) return;
    setStage("downloading");
    setProgress(0);
    try {
      let downloaded = 0;
      let total = 0;
      await update.download((event) => {
        // Progress arrives as started → chunk(s) → finished (plugin-updater).
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setProgress(total > 0 ? Math.round((downloaded / total) * 100) : 0);
        }
      });
      setProgress(100);
      setStage("ready");
    } catch (err) {
      // Failed download → back to available so the doctor can retry.
      setStage("available");
      pushToast(`Update download failed: ${String(err)}`, "error");
    }
  }

  async function installAndRestart() {
    if (!update) return;
    setStage("installing");
    try {
      await update.install();
      await relaunch();
    } catch (err) {
      // Install failed — the downloaded update is still staged, so offer Install
      // again rather than forcing a fresh download.
      setStage("ready");
      pushToast(`Update install failed: ${String(err)}`, "error");
    }
  }

  const base =
    "rounded-md px-3 py-1.5 text-xs font-medium transition-colors disabled:cursor-not-allowed";

  switch (stage) {
    case "available":
      return (
        <button
          type="button"
          onClick={startDownload}
          className={`${base} bg-teal-600/20 text-teal-300 hover:bg-teal-600/30`}
          title={`Version ${update.version} is available`}
        >
          Update available
        </button>
      );
    case "downloading":
      return (
        <button
          type="button"
          disabled
          className={`${base} bg-neutral-800 text-neutral-400 disabled:opacity-100`}
        >
          {progress > 0 ? `Downloading… ${progress}%` : "Downloading…"}
        </button>
      );
    case "ready":
      return (
        <button
          type="button"
          onClick={installAndRestart}
          className={`${base} bg-teal-600 text-white hover:bg-teal-500`}
          title="Install the update and restart the app"
        >
          Install &amp; restart
        </button>
      );
    case "installing":
      return (
        <button
          type="button"
          disabled
          className={`${base} bg-neutral-800 text-neutral-400 disabled:opacity-100`}
        >
          Installing…
        </button>
      );
  }
}
