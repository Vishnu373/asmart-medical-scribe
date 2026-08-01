import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import App from "@/App";
import { useAppStore } from "@/state";

// Bridge calls go through `invoke`. The mock is command-aware because mounting a
// view (e.g. Settings) fires its own loads — `list_input_devices` must return an
// array (the real backend always does), or `devices.map` throws during render.
// `respond` is hoisted so individual tests can override one command while reusing
// these defaults.
const { respond } = vi.hoisted(() => ({
  respond: (cmd: string): Promise<unknown> => {
    switch (cmd) {
      case "ping":
        return Promise.resolve("pong: ready");
      case "list_input_devices":
        return Promise.resolve([]);
      case "get_llm_status":
        return Promise.resolve("ready");
      case "setup_status":
        return Promise.resolve({
          llm_present: true,
          stt_present: true,
          ready: true,
        });
      case "get_settings":
        return Promise.resolve({
          mic_device: null,
          vad_threshold: 0.5,
          idle_timeout: 30,
        });
      default:
        return Promise.resolve(null);
    }
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn((cmd: string) => respond(cmd)) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

beforeEach(() => {
  useAppStore.setState({ view: "recording" });
  vi.mocked(invoke).mockImplementation((cmd) => respond(cmd) as Promise<never>);
});

describe("App shell", () => {
  it("renders the header and primary nav", async () => {
    render(<App />);
    // Setup gate (D3) resolves ready → the shell mounts after the status check.
    expect(await screen.findByRole("heading", { name: "ASmart Medical Scribe" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Primary" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Recording" })).toBeInTheDocument();
  });

  it("shows the Recording view by default", async () => {
    render(<App />);
    expect(await screen.findByRole("region", { name: "Recording" })).toBeInTheDocument();
  });

  it("switches view when a nav item is clicked", async () => {
    render(<App />);
    await userEvent.click(await screen.findByRole("button", { name: "Settings" }));
    expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Recording" })).not.toBeInTheDocument();
  });
});
