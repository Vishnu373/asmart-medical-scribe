import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import SoapEditor from "@/components/SoapEditor";
import type { Note } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue(null) }));
const mockInvoke = vi.mocked(invoke);

const note: Note = {
  id: "n1",
  record_id: "r1",
  soap_data: "## Subjective\ncough\n\n## Objective\n\n## Assessment\n\n## Plan\nrest",
  created_at: 0,
  is_active: true,
};

beforeEach(() => mockInvoke.mockClear().mockResolvedValue(null));

describe("SoapEditor", () => {
  it("renders the four sections parsed from soap_data", () => {
    render(<SoapEditor note={note} />);
    expect(screen.getByRole("textbox", { name: "Subjective" })).toHaveValue("cough");
    expect(screen.getByRole("textbox", { name: "Plan" })).toHaveValue("rest");
    expect(screen.getByRole("textbox", { name: "Objective" })).toHaveValue("");
  });

  it("debounce-saves edits via update_note with reassembled markdown", async () => {
    vi.useFakeTimers();
    render(<SoapEditor note={note} />);
    fireEvent.change(screen.getByRole("textbox", { name: "Objective" }), {
      target: { value: "temp 38" },
    });
    expect(mockInvoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(600);
    expect(mockInvoke).toHaveBeenCalledWith("update_note", {
      id: "n1",
      soapData: "## Subjective\ncough\n\n## Objective\ntemp 38\n\n## Assessment\n\n## Plan\nrest",
    });
    vi.useRealTimers();
  });

  it("copies a section's text to the clipboard for manual paste", () => {
    render(<SoapEditor note={note} />);
    // Buttons are in S/O/A/P order; the first (Subjective) has text, so it's enabled.
    fireEvent.click(screen.getAllByRole("button", { name: "Copy" })[0]);
    expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", { text: "cough" });
  });

  it("strips markdown on copy so the EMR gets plain text", () => {
    const md: Note = {
      ...note,
      soap_data:
        "## Subjective\nPatient reports a **sore throat** for 3 days.\n\n" +
        "## Objective\n- Temp 38.1 C\n- Throat erythematous\n\n" +
        "## Assessment\n\n## Plan\n",
    };
    render(<SoapEditor note={md} />);
    const buttons = screen.getAllByRole("button", { name: "Copy" });
    fireEvent.click(buttons[0]); // Subjective: bold stripped
    expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", {
      text: "Patient reports a sore throat for 3 days.",
    });
    fireEvent.click(buttons[1]); // Objective: bullet markers stripped
    expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", {
      text: "Temp 38.1 C\nThroat erythematous",
    });
  });

  it("flushes a pending edit on unmount", () => {
    render(<SoapEditor note={note} />);
    fireEvent.change(screen.getByRole("textbox", { name: "Plan" }), {
      target: { value: "follow up" },
    });
    cleanup();
    expect(mockInvoke).toHaveBeenCalledWith(
      "update_note",
      expect.objectContaining({ id: "n1" }),
    );
  });
});
