import type { Model } from "./domain";

/** Model IDs never start with a space, so this option value cannot name a model. */
export const SHOW_DISABLED = " show-disabled";

export type ModelGroup = { label: string; ids: string[] };

const byId = (left: Model, right: Model) => left.id.localeCompare(right.id);

/**
 * Groups a picker's models as Loaded, Enabled and (when shown) Disabled. A selected model stays
 * listed even while its group is hidden, so the control never shows a value it does not offer.
 */
export function modelGroups(models: Model[], showDisabled: boolean, selected = ""): { groups: ModelGroup[]; hidden: number } {
  const loaded = models.filter((model) => model.loaded).sort(byId);
  const enabled = models.filter((model) => !model.loaded && model.policy !== "disabled").sort(byId);
  const disabled = models.filter((model) => !model.loaded && model.policy === "disabled").sort(byId);
  const shown = showDisabled ? disabled : disabled.filter((model) => model.id === selected);
  const groups = [
    { label: "Loaded", ids: loaded.map((model) => model.id) },
    { label: "Enabled", ids: enabled.map((model) => model.id) },
    { label: "Disabled", ids: shown.map((model) => model.id) },
  ].filter((group) => group.ids.length > 0);
  return { groups, hidden: disabled.length - shown.length };
}
