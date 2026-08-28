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
        stt_present: done,
        draft_present: done,
        ready: done,
      });
    }
    if (cmd === "get_llm_status") return Promise.resolve(llmStatus);
    return Promise.resolve(undefined);
  });
}

/** A device taking the spec-decoding update: only the optional draft is missing,
 *  so the backend already reports `ready` (it is not part of the gate). */
const updatingInstall = {
  llm_present: true,
  stt_present: true,
  draft_present: false,
  ready: true,
};

/** How many times a `download_draft` worker was asked for. */
const draftDownloads = () =>
  mockInvoke.mock.calls.filter(([cmd]) => cmd === "download_draft").length;

describe("SetupView", () => {
  it("auto-starts the missing required downloads on first run", async () => {
    // First run: none of the three required models is present.
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_present: false,
            stt_present: false,
            draft_present: false,
            ready: false,
          });
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={vi.fn()} />);

    // All required models begin downloading without a click.
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("download_llm"));
    expect(mockInvoke).toHaveBeenCalledWith("download_stt");
    expect(mockInvoke).toHaveBeenCalledWith("download_draft");
    expect(await screen.findByText("Note model")).toBeInTheDocument();
    expect(screen.getByText("Speech recognition")).toBeInTheDocument();
    expect(screen.getByText("Draft model for note generation")).toBeInTheDocument();
  });

  it("stays gated and fetches only the draft model on an updating install", async () => {
    // The spec-decoding update ships to a device that already has the other two:
    // Setup reopens, pulls the one missing file, and does not re-download the rest.
    const onReady = vi.fn();
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve(updatingInstall);
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SetupView onReady={onReady} />);

    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("download_draft"));
    expect(mockInvoke).not.toHaveBeenCalledWith("download_llm");
    expect(mockInvoke).not.toHaveBeenCalledWith("download_stt");
    // The draft is still downloading, so Setup holds even though `ready` is true.
    expect(onReady).not.toHaveBeenCalled();
  });

  it("retries a failed download before giving up on it", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "setup_status") return Promise.resolve(updatingInstall);
      return Promise.resolve(undefined);
    });

    render(<SetupView onReady={vi.fn()} />);
    await waitFor(() => expect(handlers["model-download-error"]).toBeDefined());

    // The first attempt plus MAX_RETRIES more, then the row is left to the user.
    for (let i = 0; i < 5; i += 1) {
      await act(async () => {
        handlers["model-download-error"]({ tier: "draft", message: "network down" });
      });
    }

    expect(draftDownloads()).toBe(4);
    expect(await screen.findByText("network down")).toBeInTheDocument();
  });

  it("releases the app when the optional draft model cannot be downloaded", async () => {
    // An offline clinic on the spec-decoding update: the draft never arrives, but
    // the LLM and STT are both present, so the app must not be stranded on Setup.
    const onReady = vi.fn();
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "setup_status") return Promise.resolve(updatingInstall);
      if (cmd === "get_llm_status") return Promise.resolve("ready");
      return Promise.resolve(undefined);
    });

    render(<SetupView onReady={onReady} />);
    await waitFor(() => expect(handlers["model-download-error"]).toBeDefined());
    for (let i = 0; i < 5; i += 1) {
      await act(async () => {
        handlers["model-download-error"]({ tier: "draft", message: "network down" });
      });
    }

    await waitFor(() => expect(onReady).toHaveBeenCalled());
    // Not a first run — the required models were already there (§3 telemetry).
    expect(mockInvoke).not.toHaveBeenCalledWith("mark_setup_completed");
  });

  it("releases into the app when the required set is already present", async () => {
    const onReady = vi.fn();
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "setup_status":
          return Promise.resolve({
            llm_present: true,
            stt_present: true,
            draft_present: true,
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
    expect(mockInvoke).not.toHaveBeenCalledWith("download_stt");
    expect(mockInvoke).not.toHaveBeenCalledWith("download_draft");
    // `setup_completed` marks a genuine first-run finish (§3 telemetry) — it must
    // NOT fire when models are already present (a later, ordinary launch).
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
