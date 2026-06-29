import { invoke } from "@tauri-apps/api/core";

/**
 * Typed wrappers around backend Tauri commands.
 * Expanded per phase; for B1 this proves the frontend↔backend bridge.
 */

/** Round-trips a message through the backend to verify the bridge is wired. */
export function ping(message: string): Promise<string> {
  return invoke<string>("ping", { message });
}
