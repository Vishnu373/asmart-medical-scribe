/**
 * The frontend↔backend bridge: typed `invoke` command wrappers (§9.4), typed
 * event listeners (§9.5), and the shared payload types (§9.2–9.5). Views and
 * state slices import from here, never from `@tauri-apps/api` directly, so the
 * whole IPC surface stays in one typed place.
 */

export * from "@/bridge/types";
export * from "@/bridge/commands";
export * from "@/bridge/events";
