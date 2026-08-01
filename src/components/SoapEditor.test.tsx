import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";
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

/** Reveal the raw textarea (the editor defaults to the rendered Preview). */
const enterEdit = () => fireEvent.click(screen.getByRole("tab", { name: "edit" }));

describe("SoapEditor", () => {
  it("renders the note as formatted markdown in the default Preview", () => {
    render(<SoapEditor note={note} />);
    // The `## Subjective` header renders as a real heading, not raw text.
    expect(screen.getByRole("heading", { name: "Subjective" })).toBeInTheDocument();
    expect(screen.queryByText("## Subjective")).not.toBeInTheDocument();
    // No editable textarea until the clinician switches to Edit.
    expect(screen.queryByRole("textbox", { name: "SOAP note" })).not.toBeInTheDocument();
  });

  it("shows the raw note in a single editable window in Edit mode", () => {
    render(<SoapEditor note={note} />);
    enterEdit();
    expect(screen.getByRole("textbox", { name: "SOAP note" })).toHaveValue(note.soap_data);
  });

  it("debounce-saves edits via update_note with the verbatim note", async () => {
    vi.useFakeTimers();
    render(<SoapEditor note={note} />);
    enterEdit();
    const edited = note.soap_data.replace("## Objective\n", "## Objective\ntemp 38\n");
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: edited },
    });
    expect(mockInvoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(600);
    expect(mockInvoke).toHaveBeenCalledWith("update_note", { id: "n1", soapData: edited });
    vi.useRealTimers();
  });

  it("copies the whole note as plain text for manual paste", async () => {
    const md: Note = {
      ...note,
      soap_data:
        "## Subjective\nPatient reports a **sore throat** for 3 days.\n\n" +
        "## Objective\n- Temp 38.1 C\n- Throat erythematous\n\n" +
        "## Assessment\n\n## Plan\n",
    };
    render(<SoapEditor note={md} />);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    // Headers lose their `##`, bold is stripped, but `- ` bullets survive —
    // a plain EMR textarea can't draw them the way Preview does.
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", {
        text:
          "Subjective\nPatient reports a sore throat for 3 days.\n\n" +
          "Objective\n- Temp 38.1 C\n- Throat erythematous\n\n" +
          "Assessment\n\nPlan",
      }),
    );
    // Nothing was edited, so the flush must not have written to the DB.
    expect(mockInvoke.mock.calls.map((c) => c[0])).toEqual(["copy_to_clipboard"]);
  });

  it("copies the edited text, not the note it was rendered with", async () => {
    render(<SoapEditor note={note} />);
    enterEdit();
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: "## Objective\n- **Temp 38.1 C**" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("copy_to_clipboard", {
        text: "Objective\n- Temp 38.1 C",
      }),
    );
  });

  it("saves a pending edit before copying it", async () => {
    render(<SoapEditor note={note} />);
    enterEdit();
    const edited = note.soap_data + "\nfollow up";
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: edited },
    });
    // Copy lands inside the 600ms debounce window: the flush must beat it to the
    // DB, or the clipboard hands over text no record has.
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() =>
      expect(mockInvoke.mock.calls.map((c) => c[0])).toEqual(["update_note", "copy_to_clipboard"]),
    );
    expect(mockInvoke).toHaveBeenCalledWith("update_note", { id: "n1", soapData: edited });
  });

  // The save is awaited, not just issued first: if it fails, the clipboard must
  // not end up holding text the record does not have.
  it("does not copy when the pending save fails", async () => {
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === "update_note" ? Promise.reject(new Error("db locked")) : Promise.resolve(null),
    );
    render(<SoapEditor note={note} />);
    enterEdit();
    fireEvent.change(screen.getByRole("textbox", { name: "SOAP note" }), {
      target: { value: note.soap_data + "\nfollow up" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("update_note", expect.anything()));
    expect(mockInvoke).not.toHaveBeenCalledWith("copy_to_clipboard", expect.anything());
  });

  it("flushes a pending edit on unmount", () => {
    render(<SoapEditor note={note} />);
    enterEdit();
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
