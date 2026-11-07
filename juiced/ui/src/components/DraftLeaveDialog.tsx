import { Dialog } from "@base-ui/react/dialog";
import { Button } from "./t3/button";
import "./DraftLeaveDialog.css";

export function DraftLeaveDialog({ open, title, description, onKeepEditing, onDiscard }: {
  open: boolean;
  title: string;
  description: string;
  onKeepEditing: () => void;
  onDiscard: () => void;
}) {
  return <Dialog.Root open={open} onOpenChange={next => { if (!next) onKeepEditing(); }}>
    <Dialog.Portal>
      <Dialog.Backdrop className="dialog-backdrop" />
      <Dialog.Popup className="draft-leave-dialog">
        <Dialog.Title>{title}</Dialog.Title>
        <Dialog.Description>{description}</Dialog.Description>
        <div>
          <Button variant="outline" onClick={onKeepEditing}>Keep editing</Button>
          <Button onClick={onDiscard}>Discard changes</Button>
        </div>
      </Dialog.Popup>
    </Dialog.Portal>
  </Dialog.Root>;
}
