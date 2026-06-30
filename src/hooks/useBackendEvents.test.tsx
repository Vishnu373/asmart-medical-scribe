import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, act } from "@testing-library/react";
import { useBackendEvents } from "@/hooks/useBackendEvents";
import { useAppStore } from "@/state";

// Capture the handlers the hook registers so we can drive them like real events.
const captured = vi.hoisted(() => {
  return {} as {
    state?: (p: { state: string }) => void;
    level?: (p: { level: number[] }) => void;
    segment?: (p: { seq: number; text: string }) => void;
    error?: (p: { code: string; message: string }) => void;
  };
});

vi.mock("@/bridge", () => ({
  onStateChanged: (h: (p: { state: string }) => void) => {
    captured.state = h;
    return Promise.resolve(() => {});
  },
  onInputLevel: (h: (p: { level: number[] }) => void) => {
    captured.level = h;
    return Promise.resolve(() => {});
  },
  onTranscriptSegment: (h: (p: { seq: number; text: string }) => void) => {
    captured.segment = h;
    return Promise.resolve(() => {});
  },
  onError: (h: (p: { code: string; message: string }) => void) => {
    captured.error = h;
    return Promise.resolve(() => {});
  },
}));

function Probe() {
  useBackendEvents();
  return null;
}

beforeEach(() =>
  useAppStore.setState({
    recordingState: "IDLE",
    inputLevel: [],
    segments: [],
    transcript: "",
    toasts: [],
  }),
);

describe("useBackendEvents wires §9.5 events into the store", () => {
  it("state-changed updates recordingState", () => {
    render(<Probe />);
    act(() => captured.state!({ state: "RECORDING" }));
    expect(useAppStore.getState().recordingState).toBe("RECORDING");
  });

  it("input-level updates the meter buckets", () => {
    render(<Probe />);
    act(() => captured.level!({ level: [0.1, 0.4] }));
    expect(useAppStore.getState().inputLevel).toEqual([0.1, 0.4]);
  });

  it("leaving RECORDING resets the meter so it doesn't freeze", () => {
    render(<Probe />);
    act(() => captured.level!({ level: [0.1, 0.4] }));
    act(() => captured.state!({ state: "PROCESSING" }));
    expect(useAppStore.getState().inputLevel).toEqual([]);
  });

  it("transcript-segment appends to the transcript in order", () => {
    render(<Probe />);
    act(() => captured.segment!({ seq: 2, text: "world" }));
    act(() => captured.segment!({ seq: 1, text: "hello" }));
    expect(useAppStore.getState().transcript).toBe("hello world");
  });

  it("error pushes a toast", () => {
    render(<Probe />);
    act(() => captured.error!({ code: "ram_guard", message: "not enough RAM" }));
    const toasts = useAppStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ kind: "error", message: "not enough RAM" });
  });
});
