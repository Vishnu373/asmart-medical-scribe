/**
 * Typed wrappers around every backend Tauri command (design §9.4). The backend
 * owns all state; these are requests, and the coordinator's guards reject illegal
 * transitions by returning `Err(String)` — which surfaces here as a rejected
 * promise the UI can toast.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  InputDevice,
  LlmStatus,
  Note,
  Record,
  RecordSummary,
  Settings,
  SetupStatus,
  // TrialStatus,
} from "@/bridge/types";

/** Round-trips a message through the backend to verify the bridge is wired. */
export function ping(message: string): Promise<string> {
  return invoke<string>("ping", { message });
}

// — Recording (§9.4) — drive the IDLE→RECORDING→PROCESSING state machine.

export function startRecording(): Promise<void> {
  return invoke("start_recording");
}

/** Stop, drain, save; resolves with the saved record id (`null` if empty). */
export function stopRecording(): Promise<string | null> {
  return invoke<string | null>("stop_recording");
}

export function pauseRecording(): Promise<void> {
  return invoke("pause_recording");
}

export function resumeRecording(): Promise<void> {
  return invoke("resume_recording");
}

// — Transcript (§9.4)

export function updateTranscript(id: string, transcript: string): Promise<void> {
  return invoke("update_transcript", { id, transcript });
}

// — Notes (§8.4–8.5)

/** Generate a SOAP note; streams `generation-token`; resolves with the new note id (`null` if cancelled). */
export function generateNote(recordId: string): Promise<string | null> {
  return invoke<string | null>("generate_note", { recordId });
}

export function regenerateNote(recordId: string): Promise<string | null> {
  return invoke<string | null>("regenerate_note", { recordId });
}

export function cancelGeneration(): Promise<void> {
  return invoke("cancel_generation");
}

export function updateNote(id: string, soapData: string): Promise<void> {
  return invoke("update_note", { id, soapData });
}

export function revertVersion(recordId: string, noteId: string): Promise<void> {
  return invoke("revert_version", { recordId, noteId });
}

/** A record's note versions, newest first; the `is_active` one is current (§8.5). */
export function listNotes(recordId: string): Promise<Note[]> {
  return invoke<Note[]>("list_notes", { recordId });
}

// — Records (FR-13)

export function listRecords(): Promise<RecordSummary[]> {
  return invoke<RecordSummary[]>("list_records");
}

export function openRecord(id: string): Promise<Record | null> {
  return invoke<Record | null>("open_record", { id });
}

/** Permanent (NFR-9). */
export function deleteRecord(id: string): Promise<void> {
  return invoke("delete_record", { id });
}

// — Settings (§9.3). `update_settings` takes the full object (read-modify-write),
// so internal keys are preserved across the round-trip.

export function getSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

export function updateSettings(settings: Settings): Promise<void> {
  return invoke("update_settings", { settings });
}

/** Current note-model readiness (§8.2 startup fix). Queried once at mount to seed the
 * UI before the async `llm-status` event arrives; `"loading"` while the co-resident
 * preload warms the model, else `"ready"`. */
export function getLlmStatus(): Promise<LlmStatus> {
  return invoke<LlmStatus>("get_llm_status");
}

/** Signal the backend that the frontend has fully mounted (§8.2 startup fix), so the
 * co-resident note-model warm can begin *after* the window has painted rather than
 * during launch (which ghosted the window as "not responding"). Idempotent on the
 * backend. Fire-and-forget. */
export function frontendReady(): Promise<void> {
  return invoke("frontend_ready");
}

/** Enumerate capture devices for the mic picker (§9.3, FR-12). */
export function listInputDevices(): Promise<InputDevice[]> {
  return invoke<InputDevice[]>("list_input_devices");
}

// — First-run setup (§8.2, D3). The installer ships no model weights; the required
// set (the Gemma note model + Parakeet STT) is downloaded once on first launch, then
// cached. Each download returns once the worker is spawned — progress and the
// terminal result arrive as `model-download-*` events.

/** Whether the required models are present so the app can start (else show Setup). */
export function setupStatus(): Promise<SetupStatus> {
  return invoke<SetupStatus>("setup_status");
}

/** Begin downloading the note-generation model; progress arrives as `model-download-*`
 * events keyed by tier `"llm"`. Parameterless — there is one model. */
export function downloadLlm(): Promise<void> {
  return invoke("download_llm");
}

/** Begin downloading the Parakeet STT model; progress arrives as `model-download-*`
 * events keyed by tier `"stt"`. The archive is verified and extracted server-side. */
export function downloadStt(): Promise<void> {
  return invoke("download_stt");
}

/** Begin downloading the speculative-decoding draft model (spec-decoding B1);
 * progress arrives as `model-download-*` events keyed by tier `"draft"`. */
export function downloadDraft(): Promise<void> {
  return invoke("download_draft");
}

// — Hand-off (§8.6)

/** Copy a SOAP section's plain text to the clipboard for manual EMR paste (F7 interim). */
export function copyToClipboard(text: string): Promise<void> {
  return invoke("copy_to_clipboard", { text });
}

// — Feedback (§10.3). Doctor-typed "report a problem" text, routed through the
// telemetry seam to the same backend as crashes. Free text is NOT scrubbable, so
// the form warns against including patient information.

/** Submit a free-text problem report; rejects on an empty message. */
export function submitFeedback(message: string): Promise<void> {
  return invoke("submit_feedback", { message });
}

/**
 * Mark first-run setup as complete (§3 telemetry). Fired once, from the setup
 * screen's completion transition — a PHI-free product event, best-effort.
 */
export function markSetupCompleted(): Promise<void> {
  return invoke("mark_setup_completed");
}

// — Trial gate removed: the beta no longer expires, so nothing queries it.
// /** The beta trial verdict — whether the compiled-in end date has passed. */
// export function trialStatus(): Promise<TrialStatus> {
//   return invoke<TrialStatus>("trial_status");
// }

// — App update logging (§10.3 `[UPDATE]` rows). The updater is driven from the
// frontend, so each lifecycle transition is reported to the backend for the on-device
// log (and telemetry on the two failure stages). Binary-only, no PHI. Fire-and-forget.

/** One of the §10.3 `[UPDATE]` lifecycle stages. */
export type UpdateStage =
  | "available"
  | "downloaded"
  | "download_failed"
  | "installed"
  | "install_failed";

/** Log an app-update lifecycle event on-device (`message` = the error string for the
 * `*_failed` stages, sanitized backend-side before either sink). Best-effort. */
export function logUpdateEvent(stage: UpdateStage, message?: string): Promise<void> {
  return invoke("log_update_event", { stage, message: message ?? null });
}
