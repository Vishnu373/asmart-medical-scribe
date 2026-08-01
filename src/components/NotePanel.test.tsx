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
    llmStatus: "ready",
  });
});

describe("NotePanel", () => {
  it("generates a note then loads the active note via list_notes", async () => {
    render(<NotePanel />);
    await userEvent.click(screen.getByRole("button", { name: "Generate note" }));
    expect(mockInvoke).toHaveBeenCalledWith("generate_note", { recordId: "r1" });
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("list_notes", { recordId: "r1" }));
    // The active note loads into the editor, shown rendered (Preview default).
    expect(await screen.findByRole("heading", { name: "Subjective" })).toBeInTheDocument();
  });

  it("shows the streaming view and a Cancel button during GENERATING", async () => {
    useAppStore.setState({ recordingState: "GENERATING", streamingNote: "## Subjective\nco" });
    render(<NotePanel />);
    expect(screen.getByLabelText("Streaming note")).toHaveTextContent("co");

    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mockInvoke).toHaveBeenCalledWith("cancel_generation");
  });

  // §F2: state flips to IDLE before regenerate resolves, so Copy must stay off
  // until the refreshed note list lands — otherwise it copies the stale version.
  it("disables Copy until the post-generate list_notes refresh lands", async () => {
    let resolveGen!: (id: string) => void;
    let resolveList!: (notes: Note[]) => void;
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "regenerate_note":
          return new Promise((r) => (resolveGen = r as (id: string) => void));
        case "list_notes":
          return new Promise((r) => (resolveList = r as (notes: Note[]) => void));
        default:
          return Promise.resolve(null);
      }
    });
    useAppStore.setState({ notes: [note("n1", true)] });
    render(<NotePanel />);

    const copy = screen.getByRole("button", { name: "Copy" });
    expect(copy).toBeEnabled();

    await userEvent.click(screen.getByRole("button", { name: "Regenerate" }));
    expect(copy).toBeDisabled();

    resolveGen("n2");
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("list_notes", { recordId: "r1" }));
    expect(copy).toBeDisabled(); // refresh still in flight — note is still n1

    resolveList([note("n2", true)]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Copy" })).toBeEnabled());
  });

  // A failed refresh leaves the editor on the superseded note, so Copy must not
  // come back just because the generate itself succeeded.
  it("keeps Copy off when the post-generate refresh fails", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "regenerate_note") return Promise.resolve("n2");
      if (cmd === "list_notes") return Promise.reject(new Error("db locked"));
      return Promise.resolve(null);
    });
    useAppStore.setState({ notes: [note("n1", true)] });
    render(<NotePanel />);

    await userEvent.click(screen.getByRole("button", { name: "Regenerate" }));
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("list_notes", { recordId: "r1" }));
    expect(screen.getByRole("button", { name: "Copy" })).toBeDisabled();
  });

  it("reverts to an earlier version via revert_version", async () => {
    useAppStore.setState({ notes: [note("n2", true), note("n1", false)] });
    render(<NotePanel />);
    await userEvent.click(screen.getByRole("button", { name: "Revert" }));
    expect(mockInvoke).toHaveBeenCalledWith("revert_version", { recordId: "r1", noteId: "n1" });
  });
});
