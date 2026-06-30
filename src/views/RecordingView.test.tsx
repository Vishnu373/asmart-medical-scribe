import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import RecordingView from "@/views/RecordingView";
import { useAppStore } from "@/state";
import type { AppState } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue(null) }));
const mockInvoke = vi.mocked(invoke);

const reset = (state: AppState = "IDLE", paused = false) =>
  useAppStore.setState({ recordingState: state, paused, inputLevel: [], currentRecordId: null });

beforeEach(() => {
  mockInvoke.mockClear().mockResolvedValue(null);
  reset();
});

describe("RecordingView controls", () => {
  it("IDLE shows Start and dispatches start_recording", async () => {
    render(<RecordingView />);
    await userEvent.click(screen.getByRole("button", { name: "Start recording" }));
    expect(mockInvoke).toHaveBeenCalledWith("start_recording");
  });

  it("RECORDING shows Pause + Stop; Stop dispatches and stores the record id", async () => {
    mockInvoke.mockResolvedValueOnce("rec42"); // stop_recording → record id
    reset("RECORDING");
    render(<RecordingView />);

    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(mockInvoke).toHaveBeenCalledWith("stop_recording");
    expect(useAppStore.getState().currentRecordId).toBe("rec42");
  });

  it("pausing flips to Resume and marks paused", async () => {
    reset("RECORDING");
    render(<RecordingView />);
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(mockInvoke).toHaveBeenCalledWith("pause_recording");
    expect(useAppStore.getState().paused).toBe(true);
  });

  it("a rejected command surfaces a toast", async () => {
    mockInvoke.mockRejectedValueOnce("already recording");
    render(<RecordingView />);
    await userEvent.click(screen.getByRole("button", { name: "Start recording" }));
    expect(useAppStore.getState().toasts.at(-1)?.message).toBe("already recording");
  });
});

describe("RecordingView status + meter", () => {
  it("status label follows recordingState", () => {
    reset("PROCESSING");
    render(<RecordingView />);
    expect(screen.getByRole("status")).toHaveTextContent("Processing");
  });

  it("shows Paused when recording is paused", () => {
    reset("RECORDING", true);
    render(<RecordingView />);
    expect(screen.getByRole("status")).toHaveTextContent("Paused");
  });

  it("meter renders one bar per input-level bucket", () => {
    reset("RECORDING");
    useAppStore.setState({ inputLevel: [0.2, 0.8, 0.5] });
    render(<RecordingView />);
    const meter = screen.getByRole("meter", { name: "Input level" });
    expect(within(meter).getAllByTestId("level-bar")).toHaveLength(3);
  });
});
