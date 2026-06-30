import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "@/App";
import { useAppStore } from "@/state";

// Bridge calls go through `invoke`; the shell only pings on mount.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue("pong: ready") }));

beforeEach(() => useAppStore.setState({ view: "recording" }));

describe("App shell", () => {
  it("renders the header and primary nav", () => {
    render(<App />);
    expect(screen.getByRole("heading", { name: "Medical Scribe" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Primary" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Recording" })).toBeInTheDocument();
  });

  it("shows the Recording view by default", () => {
    render(<App />);
    expect(screen.getByRole("region", { name: "Recording" })).toBeInTheDocument();
  });

  it("switches view when a nav item is clicked", async () => {
    render(<App />);
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByRole("region", { name: "Settings" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Recording" })).not.toBeInTheDocument();
  });
});
