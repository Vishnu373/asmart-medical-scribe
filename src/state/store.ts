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

/** Cap on stacked toasts; a burst beyond this drops the oldest. */
const MAX_TOASTS = 3;

interface UiSlice {
  view: View;
  setView: (view: View) => void;
}

interface RecordingSlice {
  /** Current state-machine state, driven by `state-changed` (§9.5). */
  recordingState: AppState;
  /**
   * Whether a RECORDING session is paused. Tracked client-side because the
   * backend has no PAUSED state — it stays RECORDING and emits no event on
   * pause/resume (coordinator.rs §9.5), so the UI owns this flag.
   */
  paused: boolean;
  /** Latest mic spectrum buckets for the meter (FR-12). */
  inputLevel: number[];
  /** The record being recorded/edited, if any. */
  currentRecordId: string | null;
  setRecordingState: (state: AppState) => void;
  setPaused: (paused: boolean) => void;
  setInputLevel: (level: number[]) => void;
  setCurrentRecordId: (id: string | null) => void;
}

interface TranscriptSlice {
  /** Streamed segments for the live view (§9.5), kept in `seq` order. */
  segments: TranscriptSegmentEvent[];
  /** The editable transcript text (saved via `update_transcript`). */
  transcript: string;
  /** Append a streamed segment in `seq` order; mirrors it into `transcript`. */
  addSegment: (segment: TranscriptSegmentEvent) => void;
  setSegments: (segments: TranscriptSegmentEvent[]) => void;
  setTranscript: (transcript: string) => void;
}

interface NotesSlice {
  /** All note versions for the open record, newest first (§8.5). */
  notes: Note[];
  /** Tokens accumulated during GENERATING (§8.5). */
  streamingNote: string;
  setNotes: (notes: Note[]) => void;
  setStreamingNote: (text: string) => void;
  /** Append a streamed `generation-token` to the live note (§9.5). */
  appendStreamingToken: (text: string) => void;
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

export interface Toast {
  id: string;
  kind: "error" | "info";
  message: string;
  /** Number of coalesced occurrences of this same kind+message. */
  count: number;
}

interface ToastSlice {
  /** Transient notifications; `error` events (§9.5) and failed commands surface here. */
  toasts: Toast[];
  pushToast: (message: string, kind?: Toast["kind"]) => void;
  dismissToast: (id: string) => void;
}

export type AppStore = UiSlice &
  RecordingSlice &
  TranscriptSlice &
  NotesSlice &
  RecordsSlice &
  SettingsSlice &
  ToastSlice;

export const useAppStore = create<AppStore>((set) => ({
  // UI
  view: "recording",
  setView: (view) => set({ view }),

  // Recording
  recordingState: "IDLE",
  paused: false,
  inputLevel: [],
  currentRecordId: null,
  setRecordingState: (recordingState) => set({ recordingState }),
  setPaused: (paused) => set({ paused }),
  setInputLevel: (inputLevel) => set({ inputLevel }),
  setCurrentRecordId: (currentRecordId) => set({ currentRecordId }),

  // Transcript
  segments: [],
  transcript: "",
  addSegment: (segment) =>
    set((s) => {
      // Ignore a duplicate seq (STT retries), then keep segments ordered and
      // mirror them into the editable buffer. Segments only stream during
      // RECORDING, so this never clobbers post-stop manual edits.
      if (s.segments.some((x) => x.seq === segment.seq)) return s;
      const segments = [...s.segments, segment].sort((a, b) => a.seq - b.seq);
      return { segments, transcript: segments.map((x) => x.text).join(" ") };
    }),
  setSegments: (segments) => set({ segments }),
  setTranscript: (transcript) => set({ transcript }),

  // Notes
  notes: [],
  streamingNote: "",
  setNotes: (notes) => set({ notes }),
  setStreamingNote: (streamingNote) => set({ streamingNote }),
  appendStreamingToken: (text) => set((s) => ({ streamingNote: s.streamingNote + text })),

  // Records
  records: [],
  setRecords: (records) => set({ records }),

  // Settings
  settings: null,
  setSettings: (settings) => set({ settings }),

  // Toasts
  toasts: [],
  pushToast: (message, kind = "error") =>
    set((s) => {
      // Coalesce a repeat of an existing alert (e.g. one `error` per failed STT
      // segment) into a single toast with a count, refreshing its id so the
      // auto-dismiss timer restarts. Otherwise cap the stack at MAX_TOASTS,
      // dropping the oldest, so a burst can't flood the screen.
      const dup = s.toasts.find((t) => t.kind === kind && t.message === message);
      if (dup) {
        return {
          toasts: s.toasts.map((t) =>
            t === dup
              ? { ...t, count: t.count + 1, id: Math.random().toString(36).slice(2) }
              : t,
          ),
        };
      }
      const next = [...s.toasts, { id: Math.random().toString(36).slice(2), kind, message, count: 1 }];
      return { toasts: next.slice(-MAX_TOASTS) };
    }),
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));
