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
