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
  it("renders the whole note in a single editable window", () => {
    render(<SoapEditor note={note} />);
    expect(screen.getByRole("textbox", { name: "SOAP note" })).toHaveValue(note.soap_data);
  });

  it("debounce-saves edits via update_note with the verbatim note", async () => {
    vi.useFakeTimers();
    render(<SoapEditor note={note} />);
    const edited = note.soap_data.replace("## Objective\n", "## Objective\ntemp 38\n");
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: edited },
    });
    expect(mockInvoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(600);
    expect(mockInvoke).toHaveBeenCalledWith("update_note", { id: "n1", soapData: edited });
    vi.useRealTimers();
  });

  it("copies the whole note as plain text for manual paste", () => {
    const md: Note = {
      ...note,
      soap_data:
        "## Subjective\nPatient reports a **sore throat** for 3 days.\n\n" +
        "## Objective\n- Temp 38.1 C\n- Throat erythematous\n\n" +
        "## Assessment\n\n## Plan\n",
    };
    render(<SoapEditor note={md} />);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", {
      text:
        "## Subjective\nPatient reports a sore throat for 3 days.\n\n" +
        "## Objective\nTemp 38.1 C\nThroat erythematous\n\n" +
        "## Assessment\n\n## Plan",
    });
  });

  it("flushes a pending edit on unmount", () => {
    render(<SoapEditor note={note} />);
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: note.soap_data + "\nfollow up" },
    });
    cleanup();
    expect(mockInvoke).toHaveBeenCalledWith(
      "update_note",
      expect.objectContaining({ id: "n1" }),
    );
  });
});
