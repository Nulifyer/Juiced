import { useCallback } from "react";
import { bytes, type Model } from "./domain";
import { modelState, modelType, pickerView, typeLabel, type ModelType } from "./modelPicker";
import { Picker, type PickItem, type PickQuery, type PickView } from "./Picker";

function fact(model: Model): Pick<PickItem, "fact" | "tone"> {
  if (model.state === "failed") return { fact: "Failed", tone: "bad" };
  if (model.downloaded === false || model.state === "missing-weights") return { fact: "Files missing", tone: "warning" };
  if (model.state === "loading") return { fact: "Loading" };
  return { fact: model.resident_bytes ? bytes(model.resident_bytes) : undefined };
}

/**
 * Picks a registered model, grouped as Loaded, Enabled and Disabled. `leading` adds choices that
 * are not models, such as `["", "All sources"]`.
 */
export function ModelSelect({ models, value, onChange, placeholder = "Choose a model…", leading = [], disabledModels = "toggle", id, disabled, ...naming }: { models: Model[]; value: string; onChange: (id: string) => void; placeholder?: string; leading?: [string, string][]; disabledModels?: "toggle" | "never"; id?: string; disabled?: boolean; "aria-label"?: string; "aria-labelledby"?: string }) {
  const view = useCallback(({ query, chip, expanded }: PickQuery): PickView => {
    const picked = pickerView(models, { query, type: chip as ModelType | null, showDisabled: expanded, selected: value, disabledModels });
    return {
      groups: picked.groups.map((group) => ({ key: group.state, label: group.label, items: group.models.map((model) => ({ value: model.id, label: model.id, mark: modelState(model), kind: typeLabel(modelType(model)), ...fact(model) })) })),
      shown: picked.shown, total: picked.total, hidden: picked.hidden, more: `Show ${picked.hidden} disabled`,
      chips: picked.types.map((item) => ({ key: item.type, label: item.label, count: item.count })),
      columns: picked.types.length > 1,
    };
  }, [models, value, disabledModels]);
  return <Picker value={value} onChange={onChange} view={view} placeholder={placeholder} noun={["model", "models"]} searchHint="Search by name or type" chipsLabel="Model type" leading={leading} id={id} disabled={disabled} {...naming} />;
}
