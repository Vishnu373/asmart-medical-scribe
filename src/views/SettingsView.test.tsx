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
  mic_device: null,
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
    expect(await screen.findByRole("option", { name: /USB Mic/ })).toBeInTheDocument();
    // The model picker (§3 single-model) and residency control (§7 co-resident
    // always) are gone; only the mic remains.
    expect(screen.queryByLabelText("Note model")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Model residency")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Microphone")).toBeInTheDocument();
  });

  it("saves edits, preserving internal keys (read-modify-write)", async () => {
    render(<SettingsView />);
    const mic = await screen.findByLabelText<HTMLSelectElement>("Microphone");
    await userEvent.selectOptions(mic, "USB Mic");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("update_settings", {
        settings: { ...settings, mic_device: "USB Mic" },
      }),
    );
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
  });
});
