import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SettingsForm } from "./SettingsForm";
import { EMPTY_OVERRIDES } from "../lib/settings";
import { effective, ticketSources } from "../test/fixtures";

describe("SettingsForm", () => {
  it("shows inherited values with their source", () => {
    render(<SettingsForm value={EMPTY_OVERRIDES} inherited={effective} onChange={vi.fn()} />);
    expect(screen.getByRole("combobox", { name: "Comment language" })).toHaveAttribute(
      "placeholder",
      "Inherit: German (server)",
    );
    expect(screen.getByRole("combobox", { name: "Skip nits" })).toHaveDisplayValue("Inherit: No (global)");
  });

  it("sets and clears overrides", async () => {
    const onChange = vi.fn();
    const { rerender } = render(<SettingsForm value={EMPTY_OVERRIDES} inherited={effective} onChange={onChange} />);
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Skip nits" }), "yes");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, skipNits: true });

    rerender(<SettingsForm value={{ ...EMPTY_OVERRIDES, skipNits: true }} inherited={effective} onChange={onChange} />);
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Skip nits" }), "");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, skipNits: null });
  });

  it("picks, disables and inherits the ticket source", async () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <SettingsForm value={EMPTY_OVERRIDES} inherited={effective} ticketSources={ticketSources} onChange={onChange} />,
    );
    const select = screen.getByRole("combobox", { name: "Ticket source" });
    expect(select).toHaveDisplayValue("Inherit: Acme Jira (global)");

    await userEvent.selectOptions(select, "jira-1");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, ticketSourceId: "jira-1" });

    // "None" is stored as an empty string so it overrides an inherited source.
    await userEvent.selectOptions(select, "None");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, ticketSourceId: "" });
    rerender(
      <SettingsForm
        value={{ ...EMPTY_OVERRIDES, ticketSourceId: "" }}
        inherited={effective}
        ticketSources={ticketSources}
        onChange={onChange}
      />,
    );
    expect(screen.getByRole("combobox", { name: "Ticket source" })).toHaveDisplayValue("None");
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Ticket source" }), "");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, ticketSourceId: null });
  });

  it("edits project keys and the scope check", async () => {
    const onChange = vi.fn();
    render(<SettingsForm value={EMPTY_OVERRIDES} inherited={effective} onChange={onChange} />);
    expect(screen.getByRole("textbox", { name: "Jira project keys" })).toHaveAttribute(
      "placeholder",
      "Inherit: none (default)",
    );
    await userEvent.type(screen.getByRole("textbox", { name: "Jira project keys" }), "A");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, ticketProjectKeys: "A" });
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Check PR against ticket" }), "no");
    expect(onChange).toHaveBeenLastCalledWith({ ...EMPTY_OVERRIDES, ticketScopeCheck: false });
  });
});
