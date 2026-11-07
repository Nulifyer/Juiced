import { Popover } from "@base-ui/react/popover";
import { Info } from "lucide-react";
import type { ReactNode } from "react";
import "./HelpTip.css";

export function HelpTip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Popover.Root>
      <Popover.Trigger className="help-trigger" aria-label={`About ${label}`}>
        <Info size={14} aria-hidden="true" />
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Positioner className="help-positioner" sideOffset={6} collisionPadding={12}>
          <Popover.Popup className="help-popup">
            <Popover.Title className="help-title">{label}</Popover.Title>
            <div className="help-content">{children}</div>
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
  );
}
