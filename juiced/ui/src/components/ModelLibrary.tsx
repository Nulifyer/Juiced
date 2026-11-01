import { useState, type ReactNode } from "react";
import {
  DndContext,
  PointerSensor,
  KeyboardSensor,
  useSensor,
  useSensors,
  useDraggable,
  useDroppable,
  type DragEndEvent,
} from "@dnd-kit/core";
import { ContextMenu } from "@base-ui/react/context-menu";
import { Menu } from "@base-ui/react/menu";
import {
  ChevronDown,
  ChevronRight,
  MoreHorizontal,
  MessageSquare,
  Image,
  Mic,
  Volume2,
  Wrench,
  Lightbulb,
  LoaderCircle,
  Search,
} from "lucide-react";
import { Button } from "./t3/button";
import { api, bytes, type Model } from "../api";
export const policyNames = {
  hot: "Hot",
  "on-demand": "On demand",
  disabled: "Disabled",
};
export function CapabilityIcons({ model }: { model: Model }) {
  const labels = model.labels || [];
  const caps: [
    [boolean, string, typeof Mic],
    ...Array<[boolean, string, typeof Mic]>,
  ] = [
    [model.service === "Chat", "Chat", MessageSquare],
    [labels.includes("vision"), "Vision", Image],
    [labels.includes("tool-calling"), "Tool calls", Wrench],
    [labels.includes("reasoning"), "Reasoning", Lightbulb],
    [model.service === "Transcribe", "Transcription", Mic],
    [model.service === "Speak", "Speech synthesis", Volume2],
  ];
  return (
    <span className="capabilities">
      {caps
        .filter((c) => c[0])
        .map(([, label, Icon]) => (
          <span title={label + " · model/backend declaration"} key={label}>
            <Icon size={13} />
            <span className="sr-only">{label}</span>
          </span>
        ))}
    </span>
  );
}
function DropGroup({
  id,
  label,
  count,
  children,
}: {
  id: string;
  label: string;
  count: number;
  children: ReactNode;
}) {
  const { setNodeRef, isOver } = useDroppable({ id });
  const [open, setOpen] = useState(true);
  return (
    <section
      ref={setNodeRef}
      className={"model-group " + (isOver ? "drop-over" : "")}
    >
      <button
        className="shelf-heading"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
      >
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <span>{label}</span>
        <span className="count">{count}</span>
        {isOver && (
          <span className="drop-label">
            {id.startsWith("policy:")
              ? "Set " + label
              : label === "Loaded"
                ? "Load model"
                : "Unload model"}
          </span>
        )}
      </button>
      {open && children}
    </section>
  );
}
function Row({
  model,
  onAction,
  onInspect,
  selected,
  onSelect,
}: {
  model: Model;
  onAction: (model: Model, action: string) => void;
  onInspect: (model: Model) => void;
  selected: boolean;
  onSelect: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useDraggable({ id: model.id });
  const items = (Item: any) => (
    <>
      <div className="menu-label">Loading policy</div>
      {Object.entries(policyNames).map(([p, label]) => (
        <Item
          key={p}
          className="menu-item"
          onClick={() => onAction(model, "policy:" + p)}
        >
          {model.policy === p ? "✓ " : ""}
          {label}
        </Item>
      ))}
      <div className="menu-separator" />
      <Item
        className="menu-item"
        disabled={model.policy === "disabled" || model.operation?.pending}
        onClick={() => onAction(model, model.loaded ? "unload" : "load")}
      >
        {model.loaded ? "Unload" : "Load"}
      </Item>
      <Item className="menu-item" onClick={() => onInspect(model)}>
        Inspect model…
      </Item>
    </>
  );
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger render={<div />} className="model-context">
        <div
          ref={setNodeRef}
          className={"model-row " + (isDragging ? "dragging" : "")}
          style={{
            transform: transform
              ? `translate3d(${transform.x}px,${transform.y}px,0)`
              : undefined,
          }}
        >
          <input
            type="checkbox"
            aria-label={"Select " + model.id}
            checked={selected}
            onChange={onSelect}
          />
          <div className="model-body" {...attributes} {...listeners}>
            <div className="model-title">
              <span
                className={
                  "state-dot " +
                  (model.operation?.pending
                    ? "pending"
                    : model.loaded
                      ? "loaded"
                      : "")
                }
              />
              <strong title={model.id}>{model.id}</strong>
              {model.operation?.pending && (
                <LoaderCircle size={13} className="spin" />
              )}
            </div>
            <div className="model-detail">
              <span>{model.recipe}</span>
              <CapabilityIcons model={model} />
              <span className="badge">{policyNames[model.policy]}</span>
              <span>{model.operation?.state || model.state}</span>
              {model.size && <span>{bytes(model.size * 1e9)}</span>}
            </div>
            {model.operation?.error && (
              <p className="inline-error">{model.operation.error}</p>
            )}
          </div>
          <div className="row-actions">
            <Button
              size="compact"
              variant="ghost"
              disabled={model.policy === "disabled" || model.operation?.pending}
              onClick={() => onAction(model, model.loaded ? "unload" : "load")}
            >
              {model.loaded ? "Unload" : "Load"}
            </Button>
            <Menu.Root>
              <Menu.Trigger
                render={
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    aria-label={"Actions for " + model.id}
                  />
                }
              >
                <MoreHorizontal size={16} />
              </Menu.Trigger>
              <Menu.Portal>
                <Menu.Positioner align="end" sideOffset={5}>
                  <Menu.Popup className="menu-popup">
                    {items(Menu.Item)}
                  </Menu.Popup>
                </Menu.Positioner>
              </Menu.Portal>
            </Menu.Root>
          </div>
        </div>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Positioner>
          <ContextMenu.Popup className="menu-popup">
            {items(ContextMenu.Item)}
          </ContextMenu.Popup>
        </ContextMenu.Positioner>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}
export function ModelLibrary({
  models,
  policy,
  service,
  notify,
  refresh,
  onInspect,
}: {
  models: Model[];
  policy: boolean;
  service: string;
  notify: (s: string) => void;
  refresh: () => Promise<void>;
  onInspect: (m: Model) => void;
}) {
  const [q, setQ] = useState(""),
    [sort, setSort] = useState("name"),
    [selected, setSelected] = useState<Set<string>>(new Set()),
    [limit, setLimit] = useState(100);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 7 } }),
    useSensor(KeyboardSensor),
  );
  const visible = models
    .filter(
      (m) =>
        m.downloaded &&
        (service === "All services" || m.service === service) &&
        [m.id, m.recipe, m.policy, ...m.labels]
          .join(" ")
          .toLowerCase()
          .includes(q.toLowerCase()),
    )
    .sort((a, b) =>
      sort === "size"
        ? (b.size || 0) - (a.size || 0)
        : a.id.localeCompare(b.id),
    );
  async function action(m: Model, a: string) {
    try {
      if (a.startsWith("policy:"))
        await api("juiced/policy", { model: m.id, policy: a.slice(7) });
      else await api("juiced/operation", { model: m.id, action: a });
      notify(
        a.startsWith("policy:")
          ? "Saved loading policy for " + m.id
          : "Requested " + a + " for " + m.id,
      );
      await refresh();
    } catch (e) {
      notify((e as Error).message);
    }
  }
  function drop(e: DragEndEvent) {
    if (!e.over) return;
    const m = models.find((x) => x.id === e.active.id);
    if (!m) return;
    const id = String(e.over.id);
    void action(
      m,
      id.startsWith("policy:")
        ? id
        : "runtime:Loaded" === id
          ? "load"
          : "unload",
    );
  }
  async function bulk(a: string) {
    let ok = 0;
    const errors: string[] = [];
    for (const id of selected) {
      const m = models.find((x) => x.id === id);
      if (!m) continue;
      try {
        await api(
          a.startsWith("policy:") ? "juiced/policy" : "juiced/operation",
          a.startsWith("policy:")
            ? { model: id, policy: a.slice(7) }
            : { model: id, action: a },
        );
        ok++;
      } catch (e) {
        errors.push(id + ": " + (e as Error).message);
      }
    }
    await refresh();
    notify(
      `${ok} model changes accepted.` +
        (errors.length ? " " + errors.join("; ") : ""),
    );
    setSelected(new Set());
  }
  const groups = policy
    ? Object.keys(policyNames).map((p) => ({
        id: "policy:" + p,
        label: policyNames[p as keyof typeof policyNames],
        items: visible.filter((m) => m.policy === p),
      }))
    : [
        {
          id: "runtime:Loaded",
          label: "Loaded",
          items: visible.filter((m) => m.loaded || m.operation?.pending),
        },
        {
          id: "runtime:Unloaded",
          label: "Unloaded",
          items: visible.filter((m) => !m.loaded && !m.operation?.pending),
        },
      ];
  return (
    <>
      <header className="page-heading">
        <div>
          <h1>{policy ? "Loading policy" : "Models"}</h1>
          <p>
            {policy
              ? "Saved intent, separate from current runtime state."
              : "Actual runtime state, grouped by service."}
          </p>
        </div>
        <span className="count">{visible.length} models</span>
      </header>
      <div className="search-row">
        <label className="search-input">
          <Search size={16} />
          <input
            value={q}
            placeholder="Search name, backend or capability…"
            onChange={(e) => setQ(e.target.value)}
          />
        </label>
        <select
          aria-label="Sort models"
          value={sort}
          onChange={(e) => setSort(e.target.value)}
        >
          <option value="name">Name</option>
          <option value="size">Size</option>
        </select>
      </div>
      {selected.size > 0 && (
        <div className="bulk-bar">
          <span>{selected.size} selected</span>
          <select
            aria-label="Bulk operation"
            defaultValue=""
            onChange={(e) => {
              if (e.target.value) void bulk(e.target.value);
              e.target.value = "";
            }}
          >
            <option value="">Choose action…</option>
            <option value="load">Load</option>
            <option value="unload">Unload</option>
            {Object.entries(policyNames).map(([p, l]) => (
              <option key={p} value={"policy:" + p}>
                Set {l}
              </option>
            ))}
          </select>
          <Button
            variant="ghost"
            size="compact"
            onClick={() => setSelected(new Set())}
          >
            Clear
          </Button>
        </div>
      )}
      <DndContext sensors={sensors} onDragEnd={drop}>
        {groups.map((g) => (
          <DropGroup
            key={g.id}
            id={g.id}
            label={g.label}
            count={g.items.length}
          >
            {g.items.slice(0, limit).map((m) => (
              <Row
                key={m.id}
                model={m}
                onAction={action}
                onInspect={onInspect}
                selected={selected.has(m.id)}
                onSelect={() =>
                  setSelected((prev) => {
                    const s = new Set(prev);
                    s.has(m.id) ? s.delete(m.id) : s.add(m.id);
                    return s;
                  })
                }
              />
            ))}
            {g.items.length === 0 && (
              <p className="empty-state">No models in this group.</p>
            )}
            {g.items.length > limit && (
              <Button variant="ghost" onClick={() => setLimit(limit + 100)}>
                Show 100 more
              </Button>
            )}
          </DropGroup>
        ))}
      </DndContext>
      <p className="page-note">
        {policy
          ? "Drag between groups to save policy. On demand can remain loaded until explicitly unloaded. Disabled models reject new work."
          : "Drag to request load/unload. Hot models need a policy change before unloading. Right-click or use ⋯ for all actions."}
      </p>
    </>
  );
}
