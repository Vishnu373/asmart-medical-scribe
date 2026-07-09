import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import FeedbackButton from "@/components/FeedbackButton";
import { useAppStore } from "@/state";
import * as bridge from "@/bridge";

const initial = useAppStore.getState();
beforeEach(() => {
  useAppStore.setState(initial, true);
  vi.restoreAllMocks();
});

describe("FeedbackButton", () => {
  it("opens the form with the no-PHI disclaimer and submits the typed message", async () => {
    const submit = vi.spyOn(bridge, "submitFeedback").mockResolvedValue();
    render(<FeedbackButton />);

    await userEvent.click(screen.getByRole("button", { name: "Report a problem" }));
    expect(screen.getByText(/don.t include patient information/i)).toBeInTheDocument();

    await userEvent.type(screen.getByPlaceholderText("What happened?"), "note panel froze");
    await userEvent.click(screen.getByRole("button", { name: "Submit" }));

    expect(submit).toHaveBeenCalledWith("note panel froze");
    // Success closes the form and toasts.
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(useAppStore.getState().toasts.at(-1)?.kind).toBe("info");
  });

  it("disables Submit until the message is non-empty", async () => {
    render(<FeedbackButton />);
    await userEvent.click(screen.getByRole("button", { name: "Report a problem" }));
    expect(screen.getByRole("button", { name: "Submit" })).toBeDisabled();
  });
});
