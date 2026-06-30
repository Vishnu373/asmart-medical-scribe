/**
 * Typed wrappers around the backend → UI events (design §9.5). Each helper takes
 * a handler for the decoded payload and returns the Tauri `UnlistenFn` promise so
 * callers (effects in the views) can unsubscribe on cleanup.
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ErrorEvent,
  GenerationTokenEvent,
  InputLevelEvent,
  ModelDownloadProgressEvent,
  StateChangedEvent,
  TranscriptSegmentEvent,
} from "@/bridge/types";

function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload));
}

export function onTranscriptSegment(
  handler: (payload: TranscriptSegmentEvent) => void,
): Promise<UnlistenFn> {
  return on("transcript-segment", handler);
}

export function onInputLevel(handler: (payload: InputLevelEvent) => void): Promise<UnlistenFn> {
  return on("input-level", handler);
}

export function onGenerationToken(
  handler: (payload: GenerationTokenEvent) => void,
): Promise<UnlistenFn> {
  return on("generation-token", handler);
}

export function onStateChanged(handler: (payload: StateChangedEvent) => void): Promise<UnlistenFn> {
  return on("state-changed", handler);
}

export function onError(handler: (payload: ErrorEvent) => void): Promise<UnlistenFn> {
  return on("error", handler);
}

// — Optional-model download (§8.2, D1).

export function onModelDownloadProgress(
  handler: (payload: ModelDownloadProgressEvent) => void,
): Promise<UnlistenFn> {
  return on("model-download-progress", handler);
}

export function onModelDownloadDone(
  handler: (payload: { tier: string }) => void,
): Promise<UnlistenFn> {
  return on("model-download-done", handler);
}

export function onModelDownloadError(
  handler: (payload: { tier: string; message: string }) => void,
): Promise<UnlistenFn> {
  return on("model-download-error", handler);
}
