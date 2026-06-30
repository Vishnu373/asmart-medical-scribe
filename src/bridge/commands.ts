/**
 * Typed wrappers around every backend Tauri command (design §9.4). The backend
 * owns all state; these are requests, and the coordinator's guards reject illegal
 * transitions by returning `Err(String)` — which surfaces here as a rejected
 * promise the UI can toast.
 */

import { invoke } from "@tauri-apps/api/core";
import type { Note, Record, RecordSummary, Settings, SoapSection } from "@/bridge/types";

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

// — Hand-off (§8.6)

export function pasteSection(recordId: string, section: SoapSection): Promise<void> {
  return invoke("paste_section", { recordId, section });
}
