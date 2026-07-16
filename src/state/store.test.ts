import { describe, it, expect, beforeEach } from "vitest";
import { useAppStore } from "@/state";

const initial = useAppStore.getState();
beforeEach(() => useAppStore.setState(initial, true));

describe("app store slices", () => {
  it("starts with sensible defaults", () => {
    const s = useAppStore.getState();
    expect(s.view).toBe("recording");
    expect(s.recordingState).toBe("IDLE");
    expect(s.segments).toEqual([]);
    expect(s.records).toEqual([]);
    expect(s.settings).toBeNull();
    // Safe default: gate Generate + show the "preparing" hint until the mount seed
    // reports the true state, so a co-resident cold start can't be clicked into a
    // blocking load (§8.2 startup fix).
    expect(s.llmStatus).toBe("loading");
    expect(s.llmStatusLive).toBe(false);
  });

  it("setters update their slice", () => {
    useAppStore.getState().setView("settings");
    expect(useAppStore.getState().view).toBe("settings");

    useAppStore.getState().setRecordingState("RECORDING");
    expect(useAppStore.getState().recordingState).toBe("RECORDING");

    useAppStore.getState().setCurrentRecordId("r1");
    expect(useAppStore.getState().currentRecordId).toBe("r1");

    useAppStore.getState().setStreamingNote("## Subjective");
    expect(useAppStore.getState().streamingNote).toBe("## Subjective");
  });
});

describe("transcript slice", () => {
  it("orders segments by seq and mirrors them into the transcript", () => {
    const { addSegment } = useAppStore.getState();
    addSegment({ seq: 2, text: "world" });
    addSegment({ seq: 1, text: "hello" });
    const s = useAppStore.getState();
    expect(s.segments.map((x) => x.seq)).toEqual([1, 2]);
    expect(s.transcript).toBe("hello world");
  });

  it("ignores a duplicate seq", () => {
    const { addSegment } = useAppStore.getState();
    addSegment({ seq: 1, text: "hello" });
    addSegment({ seq: 1, text: "hello-again" });
    expect(useAppStore.getState().segments).toHaveLength(1);
  });
});

describe("notes slice", () => {
  it("appendStreamingToken accumulates the live note", () => {
    const { appendStreamingToken } = useAppStore.getState();
    appendStreamingToken("## Sub");
    appendStreamingToken("jective");
    expect(useAppStore.getState().streamingNote).toBe("## Subjective");
  });

  it("seedLlmStatus applies before the live event has arrived", () => {
    useAppStore.getState().seedLlmStatus("loading");
    expect(useAppStore.getState().llmStatus).toBe("loading");
  });

  it("a late seed cannot clobber a status the live event already advanced", () => {
    // The mount query races the `llm-status` event: a fast preload delivers `ready`
    // first, then the stale `getLlmStatus()` promise resolves with `loading`. The
    // seed must be ignored so Generate isn't stuck disabled for the session.
    useAppStore.getState().setLlmStatus("ready");
    useAppStore.getState().seedLlmStatus("loading");
    expect(useAppStore.getState().llmStatus).toBe("ready");
  });
});

describe("toast slice coalesces and caps", () => {
  it("collapses repeat alerts into one toast with a count", () => {
    const { pushToast } = useAppStore.getState();
    pushToast("transcription failed", "error");
    pushToast("transcription failed", "error");
    pushToast("transcription failed", "error");
    const toasts = useAppStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].count).toBe(3);
  });

  it("caps the stack at 3, dropping the oldest", () => {
    const { pushToast } = useAppStore.getState();
    ["a", "b", "c", "d"].forEach((m) => pushToast(m, "error"));
    const toasts = useAppStore.getState().toasts;
    expect(toasts).toHaveLength(3);
    expect(toasts.map((t) => t.message)).toEqual(["b", "c", "d"]);
  });
});
