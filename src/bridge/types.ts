/**
 * Shared types mirroring the backend's serde shapes (design §9.2–9.5).
 * Field names are snake_case to match the Rust structs exactly — these are the
 * payloads that cross the Tauri bridge, so they must line up byte-for-byte.
 */

/** Recording/generation state machine (design §6.6). Matches `RecordingState::as_str`. */
export type AppState = "IDLE" | "RECORDING" | "PROCESSING" | "GENERATING";

/** A recorded encounter with its editable transcript (no audio). `store::Record`. */
export interface Record {
  id: string;
  label: string;
  language: string;
  created_at: number;
  transcript: string;
}

/** Lightweight row for the saved-encounter list; omits the transcript. `store::RecordSummary`. */
export interface RecordSummary {
  id: string;
  label: string;
  language: string;
  created_at: number;
}

/** A generated SOAP note; many per record, exactly one active. `store::Note`. */
export interface Note {
  id: string;
  record_id: string;
  soap_data: string;
  created_at: number;
  is_active: boolean;
}

/** Doctor-facing + internal settings (design §9.3). `settings::Settings`. */
export interface Settings {
  mic_device: string | null;
  vad_threshold: number;
  idle_timeout: number;
}

/** The four SOAP sections handed off to the EMR (design §8.6). */
export type SoapSection = "subjective" | "objective" | "assessment" | "plan" | "response";

/** A selectable microphone for the settings picker. `commands::InputDevice`. */
export interface InputDevice {
  name: string;
  is_default: boolean;
}

/** Whether the required models are on disk so the app can start (D3 first-run gate). `models::SetupStatus`. */
export interface SetupStatus {
  llm_present: boolean;
  stt_present: boolean;
  /** Both required models present — the app can leave Setup. */
  ready: boolean;
}

// /** Compiled-in beta trial verdict (implementation.md §1). `trial::TrialStatus`. */
// export interface TrialStatus {
//   /** True once the trial end date has passed — the app blocks with an expired screen. */
//   expired: boolean;
//   /** Last usable day, `YYYY-MM-DD`, for the expired screen to display. */
//   end_date: string;
// }

/** Progress for an in-flight optional-model download (D1). `total === 0` ⇒ unknown size. */
export interface ModelDownloadProgressEvent {
  tier: string;
  downloaded: number;
  total: number;
}

/** Backend → UI event payloads (design §9.5). */
export interface TranscriptSegmentEvent {
  seq: number;
  text: string;
}
export interface InputLevelEvent {
  /** Normalised spectrum buckets (0..1) for the live mic meter (FR-12). */
  level: number[];
}
export interface GenerationTokenEvent {
  text: string;
}
export interface StateChangedEvent {
  state: AppState;
}
/** Note-model readiness (design §8.2 startup fix, §9.5). `loading` while the
 * co-resident preload warms the model on a background thread; `ready` once it is
 * loaded. */
export type LlmStatus = "loading" | "ready" | "error";
/** `llm-status` payload; `message` carries the error text when `status === "error"`. */
export interface LlmStatusEvent {
  status: LlmStatus;
  message?: string;
}
export interface ErrorEvent {
  code: string;
  message: string;
}
