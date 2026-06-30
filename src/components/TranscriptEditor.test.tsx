import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import TranscriptEditor from "@/components/TranscriptEditor";
import { useAppStore } from "@/state";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue(null) }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockClear().mockResolvedValue(null);
  useAppStore.setState({
    segments: [],
    transcript: "",
    currentRecordId: null,
    recordingState: "IDLE",
  });
});

describe("TranscriptEditor", () => {
  it("renders the merged transcript text", () => {
    useAppStore.getState().addSegment({ seq: 1, text: "patient reports" });
    useAppStore.getState().addSegment({ seq: 2, text: "a cough" });
    render(<TranscriptEditor />);
    expect(screen.getByRole("textbox", { name: "Transcript" })).toHaveValue(
      "patient reports a cough",
    );
  });

  it("debounce-saves edits via update_transcript once a record exists", async () => {
    vi.useFakeTimers();
    useAppStore.setState({ currentRecordId: "rec1", transcript: "x" });
    render(<TranscriptEditor />);

    // fireEvent.change sets the value synchronously — no internal timers to wait
    // on, which would deadlock under frozen fake timers.
    fireEvent.change(screen.getByRole("textbox", { name: "Transcript" }), {
      target: { value: "xy" },
    });
    expect(mockInvoke).not.toHaveBeenCalled(); // still within debounce window
    await vi.advanceTimersByTimeAsync(600);
    expect(mockInvoke).toHaveBeenCalledWith("update_transcript", {
      id: "rec1",
      transcript: "xy",
    });
    vi.useRealTimers();
  });

  it("flushes a pending save on unmount instead of dropping it", () => {
    useAppStore.setState({ currentRecordId: "rec1", transcript: "x" });
    render(<TranscriptEditor />);

    fireEvent.change(screen.getByRole("textbox", { name: "Transcript" }), {
      target: { value: "xy" },
    });
    cleanup(); // unmount before the 600 ms debounce elapses
    expect(mockInvoke).toHaveBeenCalledWith("update_transcript", {
      id: "rec1",
      transcript: "xy",
    });
  });

  it("does not save while there is no record id", async () => {
    render(<TranscriptEditor />);
    await userEvent.type(screen.getByRole("textbox", { name: "Transcript" }), "z");
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("is read-only while RECORDING so segments can't be overwritten", () => {
    useAppStore.setState({ recordingState: "RECORDING", transcript: "live" });
    render(<TranscriptEditor />);
    expect(screen.getByRole("textbox", { name: "Transcript" })).toHaveAttribute("readonly");
  });
});
