import { Select } from "@base-ui/react/select";
import { Check, ChevronDown } from "lucide-react";
import "./select.css";

export type SelectOption = {
  value: string;
  label: string;
  description?: string;
  tooltip?: string;
  disabled?: boolean;
};

export function SelectField({
  id,
  label,
  value,
  options,
  disabled,
  onValueChange,
}: {
  id?: string;
  label: string;
  value: string;
  options: SelectOption[];
  disabled?: boolean;
  onValueChange: (value: string) => void;
}) {
  return (
    <Select.Root
      value={value}
      items={options}
      disabled={disabled}
      onValueChange={(next) => {
        if (next !== null) onValueChange(next);
      }}
    >
      <Select.Trigger
        id={id}
        className="select-trigger"
        aria-label={label}
        title={options.find((option) => option.value === value)?.tooltip}
      >
        <Select.Value />
        <Select.Icon>
          <ChevronDown size={15} aria-hidden="true" />
        </Select.Icon>
      </Select.Trigger>
      <Select.Portal>
        <Select.Positioner
          className="select-positioner"
          sideOffset={6}
          align="start"
          alignItemWithTrigger={false}
          collisionPadding={12}
        >
          <Select.Popup className="select-popup">
            <Select.List
              className="select-list"
              aria-label={`${label} options`}
            >
              {options.map((option) => (
                <Select.Item
                  key={option.value}
                  value={option.value}
                  disabled={option.disabled}
                  className="select-option"
                  title={option.tooltip}
                  aria-description={option.tooltip}
                >
                  <span className="select-option-copy">
                    <Select.ItemText>{option.label}</Select.ItemText>
                    {option.description && <small>{option.description}</small>}
                  </span>
                  <Select.ItemIndicator className="select-option-check">
                    <Check size={15} aria-hidden="true" />
                  </Select.ItemIndicator>
                </Select.Item>
              ))}
            </Select.List>
          </Select.Popup>
        </Select.Positioner>
      </Select.Portal>
    </Select.Root>
  );
}
