import { describe, it, expect, beforeEach, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import * as cmd from "@/bridge/commands";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => mockInvoke.mockReset());

describe("command wrappers call invoke with the right name and args", () => {
  it("no-arg recording commands pass just the command name", async () => {
    await cmd.startRecording();
    expect(mockInvoke).toHaveBeenCalledWith("start_recording");
    await cmd.cancelGeneration();
    expect(mockInvoke).toHaveBeenCalledWith("cancel_generation");
  });

  it("ping forwards the message", async () => {
    mockInvoke.mockResolvedValueOnce("pong: hi");
    await expect(cmd.ping("hi")).resolves.toBe("pong: hi");
    expect(mockInvoke).toHaveBeenCalledWith("ping", { message: "hi" });
  });

  it("stop_recording returns the saved record id (or null)", async () => {
    mockInvoke.mockResolvedValueOnce("rec1");
    await expect(cmd.stopRecording()).resolves.toBe("rec1");
    expect(mockInvoke).toHaveBeenCalledWith("stop_recording");
  });

  it("maps snake_case command names to camelCase Tauri arg keys", async () => {
    await cmd.updateTranscript("r1", "edited");
    expect(mockInvoke).toHaveBeenCalledWith("update_transcript", {
      id: "r1",
      transcript: "edited",
    });

    await cmd.generateNote("r1");
    expect(mockInvoke).toHaveBeenCalledWith("generate_note", { recordId: "r1" });

    await cmd.updateNote("n1", "## Subjective\n...");
    expect(mockInvoke).toHaveBeenCalledWith("update_note", {
      id: "n1",
      soapData: "## Subjective\n...",
    });

    await cmd.revertVersion("r1", "n2");
    expect(mockInvoke).toHaveBeenCalledWith("revert_version", { recordId: "r1", noteId: "n2" });

    await cmd.pasteSection("r1", "subjective");
    expect(mockInvoke).toHaveBeenCalledWith("paste_section", {
      recordId: "r1",
      section: "subjective",
    });
  });

  it("record + settings commands", async () => {
    await cmd.listRecords();
    expect(mockInvoke).toHaveBeenCalledWith("list_records");
    await cmd.openRecord("r1");
    expect(mockInvoke).toHaveBeenCalledWith("open_record", { id: "r1" });
    await cmd.deleteRecord("r1");
    expect(mockInvoke).toHaveBeenCalledWith("delete_record", { id: "r1" });
    await cmd.getSettings();
    expect(mockInvoke).toHaveBeenCalledWith("get_settings");
  });
});
