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
    token?: (p: { text: string }) => void;
    restart?: (p: object) => void;
    llmStatus?: (p: { status: string; message?: string }) => void;
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
  onGenerationToken: (h: (p: { text: string }) => void) => {
    captured.token = h;
    return Promise.resolve(() => {});
  },
  onGenerationRestart: (h: (p: object) => void) => {
    captured.restart = h;
    return Promise.resolve(() => {});
  },
  onLlmStatus: (h: (p: { status: string; message?: string }) => void) => {
    captured.llmStatus = h;
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
    streamingNote: "",
    llmStatus: "loading",
    llmStatusLive: false,
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

  it("generation-token accumulates into the streaming note", () => {
    render(<Probe />);
    act(() => captured.token!({ text: "## Sub" }));
    act(() => captured.token!({ text: "jective" }));
    expect(useAppStore.getState().streamingNote).toBe("## Subjective");
  });

  it("generation-restart clears the buffer so the retry does not concatenate", () => {
    render(<Probe />);
    act(() => captured.token!({ text: "abandoned partial" }));
    act(() => captured.restart!({}));
    expect(useAppStore.getState().streamingNote).toBe("");
    act(() => captured.token!({ text: "## Subjective" }));
    expect(useAppStore.getState().streamingNote).toBe("## Subjective");
  });

  it("llm-status ready flips the note-model flag off loading", () => {
    render(<Probe />);
    act(() => captured.llmStatus!({ status: "ready" }));
    expect(useAppStore.getState().llmStatus).toBe("ready");
  });

  it("llm-status error surfaces its message as a toast", () => {
    render(<Probe />);
    act(() => captured.llmStatus!({ status: "error", message: "model file missing" }));
    expect(useAppStore.getState().llmStatus).toBe("error");
    const toasts = useAppStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ kind: "error", message: "model file missing" });
  });

  it("error pushes a toast", () => {
    render(<Probe />);
    act(() => captured.error!({ code: "ram_guard", message: "not enough RAM" }));
    const toasts = useAppStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ kind: "error", message: "not enough RAM" });
  });
});
