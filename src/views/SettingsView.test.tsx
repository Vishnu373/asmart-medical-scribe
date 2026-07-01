import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import SettingsView from "@/views/SettingsView";
import { useAppStore } from "@/state";
import type { Settings } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// Settings now subscribes to model-download events; stub listen with a no-op unlisten.
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
const mockInvoke = vi.mocked(invoke);

const settings: Settings = {
  model_choice: "medium",
  mic_device: null,
  paste_hotkey: "Alt+P",
  residency_mode: "co_resident",
  residency_override: null,
  observed_total_ram: 17000000000,
  residency_calc_version: 1,
  vad_threshold: 0.5,
  idle_timeout: 30,
};

beforeEach(() => {
  mockInvoke.mockReset().mockImplementation((cmd: string) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve(settings);
      case "list_input_devices":
        return Promise.resolve([{ name: "USB Mic", is_default: true }]);
      case "model_status":
        return Promise.resolve([
          { tier: "best", present: true, optional: false },
          { tier: "medium", present: true, optional: false },
          { tier: "okay", present: false, optional: true },
        ]);
      default:
        return Promise.resolve(undefined);
    }
  });
  useAppStore.setState({ settings: null });
});

describe("SettingsView", () => {
  it("loads settings and the device list", async () => {
    render(<SettingsView />);
    await waitFor(() =>
      expect(screen.getByLabelText("Note model")).toHaveValue("medium"),
    );
    expect(await screen.findByRole("option", { name: /USB Mic/ })).toBeInTheDocument();
  });

  it("saves edits, preserving internal keys (read-modify-write)", async () => {
    render(<SettingsView />);
    const model = await screen.findByLabelText<HTMLSelectElement>("Note model");
    await userEvent.selectOptions(model, "best");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(mockInvoke).toHaveBeenCalledWith("update_settings", {
      settings: { ...settings, model_choice: "best" },
    });
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
  });

  it("offers a download for the optional model tier when it is absent", async () => {
    render(<SettingsView />);
    const download = await screen.findByRole("button", { name: "Download" });
    await userEvent.click(download);
    expect(mockInvoke).toHaveBeenCalledWith("download_model", { tier: "okay" });
  });

  it("offers Mistral + Q4 downloads on a <16 GB build (Phi Q8 bundled)", async () => {
    // A <16 GB build bundles Phi Q8; both absent tiers (Mistral "best" and Phi Q4
    // "okay") are downloadable, so each is disabled until pulled and gets a control.
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "get_settings":
          return Promise.resolve(settings);
        case "list_input_devices":
          return Promise.resolve([{ name: "USB Mic", is_default: true }]);
        case "model_status":
          return Promise.resolve([
            { tier: "best", present: false, optional: true },
            { tier: "medium", present: true, optional: false },
            { tier: "okay", present: false, optional: true },
          ]);
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SettingsView />);
    const best = await screen.findByRole("option", { name: /Best/ });
    expect(best).toBeDisabled();
    expect(best).toHaveTextContent(/download required/);
    // Both absent optional tiers get a Download control; the bundled Phi Q8 does not.
    const downloads = await screen.findAllByRole("button", { name: "Download" });
    expect(downloads).toHaveLength(2);
  });

  it("offers a download for each absent optional tier (≥16 GB build: Q8 + Q4)", async () => {
    // A ≥16 GB build bundles Mistral; both Phi tiers are absent and downloadable.
    mockInvoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "get_settings":
          return Promise.resolve(settings);
        case "list_input_devices":
          return Promise.resolve([{ name: "USB Mic", is_default: true }]);
        case "model_status":
          return Promise.resolve([
            { tier: "best", present: true, optional: false },
            { tier: "medium", present: false, optional: true },
            { tier: "okay", present: false, optional: true },
          ]);
        default:
          return Promise.resolve(undefined);
      }
    });

    render(<SettingsView />);
    const downloads = await screen.findAllByRole("button", { name: "Download" });
    expect(downloads).toHaveLength(2);
    // The first control corresponds to "medium" (MODELS order: best, medium, okay).
    await userEvent.click(downloads[0]);
    expect(mockInvoke).toHaveBeenCalledWith("download_model", { tier: "medium" });
  });
});
