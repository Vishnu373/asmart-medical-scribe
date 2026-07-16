import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import StatusBadge from "@/components/StatusBadge";

describe("StatusBadge surfaces model readiness while idle", () => {
  it("shows Loading while the model warms (§8.2)", () => {
    render(<StatusBadge state="IDLE" paused={false} llmStatus="loading" />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading");
  });

  it("shows Ready once the model is loaded", () => {
    render(<StatusBadge state="IDLE" paused={false} llmStatus="ready" />);
    expect(screen.getByRole("status")).toHaveTextContent("Ready");
  });

  it("falls back to Idle on a load error", () => {
    render(<StatusBadge state="IDLE" paused={false} llmStatus="error" />);
    expect(screen.getByRole("status")).toHaveTextContent("Idle");
  });

  it("an active state wins over the model status", () => {
    render(<StatusBadge state="RECORDING" paused={false} llmStatus="loading" />);
    expect(screen.getByRole("status")).toHaveTextContent("Recording");
  });

  it("keeps the Paused override", () => {
    render(<StatusBadge state="RECORDING" paused={true} llmStatus="ready" />);
    expect(screen.getByRole("status")).toHaveTextContent("Paused");
  });
});
