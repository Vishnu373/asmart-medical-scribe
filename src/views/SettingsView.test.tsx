import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import SettingsView from "@/views/SettingsView";
import { useAppStore } from "@/state";
import type { Settings } from "@/bridge";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
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
    // Rebinds the global hotkey live so the change applies without a restart.
    expect(mockInvoke).toHaveBeenCalledWith("rebind_paste_hotkey", { accelerator: "Alt+P" });
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
  });

  it("captures a modifier+key combo into the paste hotkey", async () => {
    render(<SettingsView />);
    const hotkey = await screen.findByLabelText<HTMLInputElement>("Paste hotkey");
    await userEvent.type(hotkey, "{Control>}k{/Control}");
    expect(hotkey).toHaveValue("Ctrl+K");
  });

  it("rejects a hotkey with no modifier", async () => {
    render(<SettingsView />);
    const hotkey = await screen.findByLabelText<HTMLInputElement>("Paste hotkey");
    await userEvent.type(hotkey, "k");
    expect(hotkey).toHaveValue("Alt+P"); // unchanged
  });
});
