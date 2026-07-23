import { useEffect } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { useAppStore } from "@/state";
import { logUpdateEvent } from "@/bridge";

/**
 * Passively check the updater endpoint for a newer release on startup and stash
 * the result in the store — nothing is downloaded or installed here. The doctor
 * drives download and install explicitly via the header update button
 * ([`UpdateButton`]); this hook only flips the stage to `available`.
 *
 * This is a binary-only network channel (no PHI). Any failure — offline, no
 * endpoint, bad signature — is silently non-fatal: the button simply never
 * appears and the app runs on the current version.
 */
export function useUpdateCheck(): void {
  const setUpdate = useAppStore((s) => s.setUpdate);
  const setUpdateStage = useAppStore((s) => s.setUpdateStage);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const update = await check();
        if (cancelled || !update) return; // up to date
        setUpdate(update);
        setUpdateStage("available");
        void logUpdateEvent("available"); // §10.3 `[UPDATE] update available`
      } catch {
        // Update problems must never block the app; leave the button hidden.
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [setUpdate, setUpdateStage]);
}
