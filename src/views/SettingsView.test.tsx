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
});
