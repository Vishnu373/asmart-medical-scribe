import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import NavBar from "@/components/NavBar";
import { useAppStore } from "@/state";

beforeEach(() => useAppStore.setState({ view: "recording", recordingState: "IDLE" }));

describe("NavBar", () => {
  it("navigates between views when idle", async () => {
    render(<NavBar />);
    await userEvent.click(screen.getByRole("button", { name: "Records" }));
    expect(useAppStore.getState().view).toBe("records");
  });

  it("locks navigation away from Recording while a session is live", async () => {
    useAppStore.setState({ recordingState: "RECORDING" });
    render(<NavBar />);

    const records = screen.getByRole("button", { name: "Records" });
    expect(records).toBeDisabled();

    await userEvent.click(records);
    expect(useAppStore.getState().view).toBe("recording");
  });
});
