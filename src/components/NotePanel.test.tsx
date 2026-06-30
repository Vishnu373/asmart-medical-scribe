import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import NotePanel from "@/components/NotePanel";
import { useAppStore } from "@/state";
import type { Note } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const note = (id: string, is_active: boolean): Note => ({
  id,
  record_id: "r1",
  soap_data: "## Subjective\nx\n\n## Objective\n\n## Assessment\n\n## Plan\n",
  created_at: 0,
  is_active,
});

beforeEach(() => {
  mockInvoke.mockReset().mockImplementation((cmd: string) => {
    switch (cmd) {
      case "generate_note":
        return Promise.resolve("n1");
      case "list_notes":
        return Promise.resolve([note("n1", true)]);
      default:
        return Promise.resolve(null);
    }
  });
  useAppStore.setState({
    recordingState: "IDLE",
    currentRecordId: "r1",
    notes: [],
    streamingNote: "",
  });
});

describe("NotePanel", () => {
  it("generates a note then loads the active note via list_notes", async () => {
    render(<NotePanel />);
    await userEvent.click(screen.getByRole("button", { name: "Generate note" }));
    expect(mockInvoke).toHaveBeenCalledWith("generate_note", { recordId: "r1" });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("list_notes", { recordId: "r1" }));
    expect(await screen.findByRole("textbox", { name: "Subjective" })).toBeInTheDocument();
  });

  it("shows the streaming view and a Cancel button during GENERATING", async () => {
    useAppStore.setState({ recordingState: "GENERATING", streamingNote: "## Subjective\nco" });
    render(<NotePanel />);
    expect(screen.getByLabelText("Streaming note")).toHaveTextContent("co");

    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mockInvoke).toHaveBeenCalledWith("cancel_generation");
  });

  it("reverts to an earlier version via revert_version", async () => {
    useAppStore.setState({ notes: [note("n2", true), note("n1", false)] });
    render(<NotePanel />);
    await userEvent.click(screen.getByRole("button", { name: "Revert" }));
    expect(mockInvoke).toHaveBeenCalledWith("revert_version", { recordId: "r1", noteId: "n1" });
  });
});
