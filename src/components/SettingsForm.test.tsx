import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SettingsForm } from "./SettingsForm";
import { EMPTY_OVERRIDES } from "../lib/settings";
import { effective } from "../test/fixtures";

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
});
