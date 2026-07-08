import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import RecordsView from "@/views/RecordsView";
import { useAppStore } from "@/state";
import type { Record, RecordSummary } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const summary = (id: string, label: string): RecordSummary => ({
  id,
  label,
  language: "en",
  created_at: 0,
});

const record = (id: string): Record => ({
  id,
  label: "Visit",
  language: "en",
  created_at: 0,
  transcript: "patient reports a cough",
});

beforeEach(() => {
  mockInvoke.mockReset().mockImplementation((cmd: string) => {
    switch (cmd) {
      case "list_records":
        return Promise.resolve([summary("r1", "Morning visit"), summary("r2", "Afternoon visit")]);
      case "open_record":
        return Promise.resolve(record("r1"));
      case "list_notes":
        return Promise.resolve([]);
      default:
        return Promise.resolve(null);
    }
  });
  useAppStore.setState({
    view: "records",
    records: [],
    currentRecordId: null,
    transcript: "",
    notes: [],
    suggestions: [],
  });
});

describe("RecordsView", () => {
  it("lists saved encounters from list_records", async () => {
    render(<RecordsView />);
    expect(await screen.findByText("Morning visit")).toBeInTheDocument();
    expect(screen.getByText("Afternoon visit")).toBeInTheDocument();
  });

  it("opens a record into the Recording view with its transcript and notes", async () => {
    render(<RecordsView />);
    await userEvent.click((await screen.findAllByRole("button", { name: "Open" }))[0]);

    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("open_record", { id: "r1" }));
    expect(mockInvoke).toHaveBeenCalledWith("list_notes", { recordId: "r1" });
    await waitFor(() => {
      const s = useAppStore.getState();
      expect(s.currentRecordId).toBe("r1");
      expect(s.transcript).toBe("patient reports a cough");
      expect(s.view).toBe("recording");
    });
  });

  it("clears prior-consult suggestions when opening a record", async () => {
    // Stale suggestions belong to a different transcript; opening a record must drop
    // them so Accept can't patch the wrong record (§6.7).
    useAppStore.setState({ suggestions: [{ original: "cough", replacement: "wheeze" }] });
    render(<RecordsView />);
    await userEvent.click((await screen.findAllByRole("button", { name: "Open" }))[0]);
    await waitFor(() => expect(useAppStore.getState().suggestions).toHaveLength(0));
  });

  it("deletes only after an inline confirm", async () => {
    render(<RecordsView />);
    await userEvent.click((await screen.findAllByRole("button", { name: "Delete" }))[0]);
    expect(mockInvoke).not.toHaveBeenCalledWith("delete_record", expect.anything());

    await userEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(mockInvoke).toHaveBeenCalledWith("delete_record", { id: "r1" });
  });
});
