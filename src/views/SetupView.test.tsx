import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import SetupView from "@/views/SetupView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// SetupView subscribes to model-download and llm-status events; capture each
// handler by event name so tests can deliver payloads, with a no-op unlisten.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

/** Captured event handlers, keyed by the `listen` event name. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let handlers: Record<string, (payload: any) => void>;

beforeEach(() => {
  mockInvoke.mockReset();
  handlers = {};
  mockListen.mockReset();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  mockListen.mockImplementation((event: string, handler: any) => {
    handlers[event] = (payload) => handler({ payload });
    return Promise.resolve(() => {});
  });
});

/** `setup_status` answers, in call order: downloads pending, then complete.
 * `llmStatus` is what `get_llm_status` reports during the priming step. */
function firstRunThenComplete(llmStatus: "loading" | "ready" = "loading") {
  let calls = 0;
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === "setup_status") {
      calls += 1;
      const done = calls > 1;
      return Promise.resolve({
        llm_present: done,
        llm_draft_present: done,
        stt_present: done,
        ready: done,
      });
    }
    if (cmd === "get_llm_status") return Promise.resolve(llmStatus);
    return Promise.resolve(undefined);
  });
}

describe("SetupView", () => {
  it("auto-starts the missing required downloads on first run", async () => {
    // First run: none of the note model, its draft, or Parakeet is present.
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_present: false,
            llm_draft_present: false,
            stt_present: false,
            ready: false,
          });
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={vi.fn()} />);

    // Every required model begins downloading without a click.
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("download_llm"));
    expect(mockInvoke).toHaveBeenCalledWith("download_llm_draft");
    expect(mockInvoke).toHaveBeenCalledWith("download_stt");
    expect(await screen.findByText("Note model")).toBeInTheDocument();
    expect(screen.getByText("Draft model")).toBeInTheDocument();
    expect(screen.getByText("Speech recognition")).toBeInTheDocument();
  });

  it("releases into the app when the required set is already present", async () => {
    const onReady = vi.fn();
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_present: true,
            llm_draft_present: true,
            stt_present: true,
            ready: true,
          });
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={onReady} />);

    await waitFor(() => expect(onReady).toHaveBeenCalled());
    // Nothing is downloaded when every model is already on disk.
    expect(mockInvoke).not.toHaveBeenCalledWith("download_llm");
    expect(mockInvoke).not.toHaveBeenCalledWith("download_llm_draft");
    expect(mockInvoke).not.toHaveBeenCalledWith("download_stt");
    // `setup_completed` marks a genuine first-run finish (§3 telemetry) — it must
    // NOT fire when models are already present (a later, ordinary launch).
    expect(mockInvoke).not.toHaveBeenCalledWith("mark_setup_completed");
  });

  it("does not re-count setup when an update only adds the draft model", async () => {
    // An upgrading install: the note and speech models are already on disk and only
    // the newly required draft is missing, so `ready` is false and Setup renders.
    let calls = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "setup_status") {
        calls += 1;
        const done = calls > 1;
        return Promise.resolve({
          llm_present: true,
          llm_draft_present: done,
          stt_present: true,
          ready: done,
        });
      }
      if (cmd === "get_llm_status") return Promise.resolve("loading");
      return Promise.resolve(undefined);
    });

    render(<SetupView onReady={vi.fn()} />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("download_llm_draft"));
    // Only the missing model is fetched.
    expect(mockInvoke).not.toHaveBeenCalledWith("download_llm");
    expect(mockInvoke).not.toHaveBeenCalledWith("download_stt");

    await waitFor(() => expect(handlers["model-download-done"]).toBeDefined());
    await act(async () => {
      handlers["model-download-done"]({ tier: "llm-draft" });
    });

    // The draft's arrival still hands over to the priming step…
    expect(await screen.findByText("Preparing note model…")).toBeInTheDocument();
    // …but this was never a first run, so the §3 counter must not move.
    expect(mockInvoke).not.toHaveBeenCalledWith("mark_setup_completed");
  });

  it("holds on the priming step after the downloads complete", async () => {
    const onReady = vi.fn();
    firstRunThenComplete();

    render(<SetupView onReady={onReady} />);
    await waitFor(() => expect(handlers["model-download-done"]).toBeDefined());
    await act(async () => {
      handlers["model-download-done"]({ tier: "llm" });
    });

    expect(await screen.findByText("Preparing note model…")).toBeInTheDocument();
    // The downloads are a genuine first-run finish, but the app is not released yet.
    expect(mockInvoke).toHaveBeenCalledWith("mark_setup_completed");
    expect(onReady).not.toHaveBeenCalled();
    // The mount-time preload failed before the weights existed; this is the retry.
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("frontend_ready"));
  });

  it("releases the app once the note model reports ready", async () => {
    const onReady = vi.fn();
    firstRunThenComplete();

    render(<SetupView onReady={onReady} />);
    await waitFor(() => expect(handlers["model-download-done"]).toBeDefined());
    await act(async () => {
      handlers["model-download-done"]({ tier: "llm" });
    });
    await waitFor(() => expect(handlers["llm-status"]).toBeDefined());
    act(() => handlers["llm-status"]({ status: "ready" }));

    expect(onReady).toHaveBeenCalled();
  });

  it("releases the app on a note-model load error rather than trapping Setup", async () => {
    const onReady = vi.fn();
    firstRunThenComplete();

    render(<SetupView onReady={onReady} />);
    await waitFor(() => expect(handlers["model-download-done"]).toBeDefined());
    await act(async () => {
      handlers["model-download-done"]({ tier: "llm" });
    });
    await waitFor(() => expect(handlers["llm-status"]).toBeDefined());
    act(() => handlers["llm-status"]({ status: "error", message: "model file missing" }));

    expect(onReady).toHaveBeenCalled();
  });

  it("releases the app when the model was already loaded, so no llm-status is coming", async () => {
    // Weights present but STT missing ⇒ the mount-time preload succeeded and latched
    // the one-shot gate, so the retried `frontend_ready` emits nothing at all.
    const onReady = vi.fn();
    firstRunThenComplete("ready");

    render(<SetupView onReady={onReady} />);
    await waitFor(() => expect(handlers["model-download-done"]).toBeDefined());
    await act(async () => {
      handlers["model-download-done"]({ tier: "stt" });
    });

    // No `llm-status` is ever delivered here: the seeded status is the only release.
    await waitFor(() => expect(onReady).toHaveBeenCalled());
  });
});
