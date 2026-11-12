import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";

export function useRevisionSelection(client: Client, model: Model, onDirty: (dirty: boolean) => void, reload: () => Promise<unknown>) {
  const [selected, setSelected] = useState("");
  const [draftRevision, setDraftRevision] = useState<number | null>(null);
  const last = useRef({ revision: model.revision, state: model.state });
  useEffect(() => { onDirty(!!selected); }, [selected, onDirty]);
  useEffect(() => { setSelected(""); setDraftRevision(null); }, [client]);
  useEffect(() => {
    if (last.current.revision !== model.revision || last.current.state !== model.state) void reload();
    last.current = { revision: model.revision, state: model.state };
  }, [model.revision, model.state, reload]);
  function select(value: string, revision: number | null) { setSelected(value); setDraftRevision(value ? revision : null); }
  return { selected, draftRevision, select, rebase: setDraftRevision, clear: () => select("", null) };
}
