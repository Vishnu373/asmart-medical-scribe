/**
 * Global app state (design §9 frontend). One Zustand store composed of focused
 * slices — recording, transcript, notes, records, settings — plus a small UI
 * slice for which view is active. F1 establishes the shape and plain setters;
 * later phases (F2–F6) wire the backend events and commands into these.
 */

import { create } from "zustand";
import type {
  AppState,
  Note,
  RecordSummary,
  Settings,
  TranscriptSegmentEvent,
} from "@/bridge";

export type View = "recording" | "records" | "settings";

interface UiSlice {
  view: View;
  setView: (view: View) => void;
}

interface RecordingSlice {
  /** Current state-machine state, driven by `state-changed` (§9.5). */
  recordingState: AppState;
  /** Latest mic spectrum buckets for the meter (FR-12). */
  inputLevel: number[];
  /** The record being recorded/edited, if any. */
  currentRecordId: string | null;
  setRecordingState: (state: AppState) => void;
  setInputLevel: (level: number[]) => void;
  setCurrentRecordId: (id: string | null) => void;
}

interface TranscriptSlice {
  /** Streamed segments for the live view (§9.5); ordering handled in F3. */
  segments: TranscriptSegmentEvent[];
  /** The editable transcript text (saved via `update_transcript`). */
  transcript: string;
  setSegments: (segments: TranscriptSegmentEvent[]) => void;
  setTranscript: (transcript: string) => void;
}

interface NotesSlice {
  /** All note versions for the open record (§8.5). */
  notes: Note[];
  /** Tokens accumulated during GENERATING (§8.5). */
  streamingNote: string;
  setNotes: (notes: Note[]) => void;
  setStreamingNote: (text: string) => void;
}

interface RecordsSlice {
  /** Saved-encounter list (FR-13). */
  records: RecordSummary[];
  setRecords: (records: RecordSummary[]) => void;
}

interface SettingsSlice {
  /** Loaded settings, `null` until first fetched (§9.3). */
  settings: Settings | null;
  setSettings: (settings: Settings | null) => void;
}

export type AppStore = UiSlice &
  RecordingSlice &
  TranscriptSlice &
  NotesSlice &
  RecordsSlice &
  SettingsSlice;

export const useAppStore = create<AppStore>((set) => ({
  // UI
  view: "recording",
  setView: (view) => set({ view }),

  // Recording
  recordingState: "IDLE",
  inputLevel: [],
  currentRecordId: null,
  setRecordingState: (recordingState) => set({ recordingState }),
  setInputLevel: (inputLevel) => set({ inputLevel }),
  setCurrentRecordId: (currentRecordId) => set({ currentRecordId }),

  // Transcript
  segments: [],
  transcript: "",
  setSegments: (segments) => set({ segments }),
  setTranscript: (transcript) => set({ transcript }),

  // Notes
  notes: [],
  streamingNote: "",
  setNotes: (notes) => set({ notes }),
  setStreamingNote: (streamingNote) => set({ streamingNote }),

  // Records
  records: [],
  setRecords: (records) => set({ records }),

  // Settings
  settings: null,
  setSettings: (settings) => set({ settings }),
}));
