import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import CorrectionSuggestions from "@/components/CorrectionSuggestions";
import { useAppStore } from "@/state";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue(null) }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockClear().mockResolvedValue(null);
  useAppStore.setState({
    recordingState: "IDLE",
    transcript: "",
    currentRecordId: null,
    suggestions: [],
  });
});

describe("CorrectionSuggestions (§6.7)", () => {
  it("renders nothing when idle with no suggestions", () => {
    const { container } = render(<CorrectionSuggestions />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows a scanning hint and Cancel while CORRECTING", () => {
    useAppStore.setState({ recordingState: "CORRECTING" });
    render(<CorrectionSuggestions />);
    expect(screen.getByText("Scanning the transcript…")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mockInvoke).toHaveBeenCalledWith("cancel_generation");
  });

  it("accepts a suggestion: patches the first span occurrence and autosaves", () => {
    useAppStore.setState({
      currentRecordId: "rec1",
      transcript: "headache right down beforehand for days",
      suggestions: [{ original: "right down beforehand", replacement: "right side of forehead" }],
    });
    render(<CorrectionSuggestions />);

    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(useAppStore.getState().transcript).toBe("headache right side of forehead for days");
    expect(mockInvoke).toHaveBeenCalledWith("update_transcript", {
      id: "rec1",
      transcript: "headache right side of forehead for days",
    });
    // The suggestion leaves the list once resolved.
    expect(useAppStore.getState().suggestions).toHaveLength(0);
  });

  it("rejects a suggestion: dismisses it without touching the transcript", () => {
    useAppStore.setState({
      currentRecordId: "rec1",
      transcript: "unchanged text",
      suggestions: [{ original: "unchanged", replacement: "changed" }],
    });
    render(<CorrectionSuggestions />);

    fireEvent.click(screen.getByRole("button", { name: "Reject" }));

    expect(useAppStore.getState().transcript).toBe("unchanged text");
    expect(useAppStore.getState().suggestions).toHaveLength(0);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("dismisses without saving when the span is no longer in the transcript", () => {
    useAppStore.setState({
      currentRecordId: "rec1",
      transcript: "already edited away",
      suggestions: [{ original: "tie the null", replacement: "Tylenol" }],
    });
    render(<CorrectionSuggestions />);

    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(useAppStore.getState().transcript).toBe("already edited away");
    expect(useAppStore.getState().suggestions).toHaveLength(0);
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
