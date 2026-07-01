import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import SetupView from "@/views/SetupView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// SetupView subscribes to model-download events; stub listen with a no-op unlisten.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
const mockInvoke = vi.mocked(invoke);

beforeEach(() => mockInvoke.mockReset());

describe("SetupView", () => {
  it("auto-starts the missing required downloads on first run", async () => {
    // <16 GB machine: needs Phi Q8 ("medium") + Parakeet; neither is present.
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_tier: "medium",
            llm_present: false,
            stt_present: false,
            ready: false,
          });
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={vi.fn()} />);

    // Both required models begin downloading without a click.
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("download_model", { tier: "medium" }),
    );
    expect(mockInvoke).toHaveBeenCalledWith("download_stt");
    expect(await screen.findByText("Note model")).toBeInTheDocument();
    expect(screen.getByText("Speech recognition")).toBeInTheDocument();
  });

  it("releases into the app when the required set is already present", async () => {
    const onReady = vi.fn();
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_tier: "best",
            llm_present: true,
            stt_present: true,
            ready: true,
          });
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={onReady} />);

    await waitFor(() => expect(onReady).toHaveBeenCalled());
    // Nothing is downloaded when both models are already on disk.
    expect(mockInvoke).not.toHaveBeenCalledWith("download_model", expect.anything());
    expect(mockInvoke).not.toHaveBeenCalledWith("download_stt");
  });
});
