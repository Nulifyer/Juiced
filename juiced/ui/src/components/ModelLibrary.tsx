import { useEffect, useState, type ReactNode } from "react";
import {
  DndContext,
  DragOverlay,
  pointerWithin,
  rectIntersection,
  MouseSensor,
  TouchSensor,
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
  MoreHorizontal,
  MessageSquare,
  Image,
  Mic,
  Volume2,
  Wrench,
  Lightbulb,
  LoaderCircle,
  Search,
  GripVertical,
  Play,
  Flame,
  Power,
  Clock3,
  Database,
  ArrowUpRight,
  SlidersHorizontal,
} from "lucide-react";
import { Button } from "./t3/button";
import { api, bytes, type Model } from "../api";
import "./ModelLibrary.css";
import { modelQuant } from "./modelDiscovery";
export const policyNames = {
  hot: "Hot",
  "on-demand": "On demand",
  disabled: "Disabled",
};
export const serviceNames: Record<string, string> = {
  Chat: "Chat",
  Transcribe: "Transcription",
  Speak: "Speech",
  Images: "Images",
};
export function ServiceIcon({
  service,
  size = 18,
}: {
  service: string;
  size?: number;
}) {
  const Icon =
    (
      {
        Chat: MessageSquare,
        Transcribe: Mic,
        Speak: Volume2,
        Images: Image,
      } as Record<string, typeof Mic>
    )[service] || Database;
  return <Icon size={size} />;
}
export function CapabilityIcons({ model }: { model: Model }) {
  const caps: [boolean, string, typeof Mic][] = [
    [model.service === "Chat", "Chat", MessageSquare],
    [model.labels.includes("vision"), "Vision", Image],
    [model.labels.includes("tool-calling"), "Tool calls", Wrench],
    [model.labels.includes("reasoning"), "Reasoning", Lightbulb],
    [model.service === "Transcribe", "Transcription", Mic],
    [model.service === "Speak", "Speech synthesis", Volume2],
    [model.service === "Images", "Image generation", Image],
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
const policies = [
  {
    id: "hot",
    title: "Hot",
    description: "Load at startup. Keep ready and recover if a worker stops.",
    Icon: Flame,
  },
  {
    id: "on-demand",
    title: "On demand",
    description: "Load when requested. Keep the weights available.",
    Icon: Clock3,
  },
  {
    id: "disabled",
    title: "Disabled",
    description: "Keep the files. Exclude the model from new requests.",
    Icon: Power,
  },
] as const;
function DropArea({
  id,
  className,
  children,
}: {
  id: string;
  className: string;
  children: ReactNode;
}) {
  const { setNodeRef, isOver } = useDroppable({ id });
  return (
    <section
      ref={setNodeRef}
      className={className + (isOver ? " drop-over" : "")}
    >
      {children}
    </section>
  );
}
function ModelRow({
  model: m,
  policy,
  selected,
  onSelect,
  onAction,
  onInspect,
  onRemove,
  onPlay,
}: {
  model: Model;
  policy: boolean;
  selected: boolean;
  onSelect: () => void;
  onAction: (m: Model, a: string) => void;
  onInspect: (m: Model) => void;
  onRemove: (m: Model) => void;
  onPlay: (m: Model) => void;
}) {
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id: m.id,
    disabled: Boolean(m.operation?.pending),
  });
  const items = (Item: any) => (
    <>
      <div className="menu-label">Loading policy</div>
      {Object.entries(policyNames).map(([p, label]) => (
        <Item
          key={p}
          className="menu-item"
          onClick={() => onAction(m, "policy:" + p)}
        >
          {m.policy === p ? "✓ " : ""}
          {label}
        </Item>
      ))}
      <div className="menu-separator" />
      <Item
        className="menu-item"
        disabled={
          m.policy === "disabled" ||
          m.operation?.pending ||
          (m.loaded && m.policy === "hot")
        }
        onClick={() => onAction(m, m.loaded ? "unload" : "load")}
      >
        {m.loaded ? "Unload model" : "Load model"}
      </Item>
      {m.loaded && (
        <Item
          className="menu-item"
          disabled={
            m.operation?.pending ||
            m.runtime?.is_busy ||
            m.policy === "disabled"
          }
          onClick={() => onAction(m, "reload")}
        >
          Reload with saved settings
        </Item>
      )}
      <Item className="menu-item" onClick={() => onInspect(m)}>
        Settings & variants…
      </Item>
      <Item className="menu-item" onClick={() => onRemove(m)}>
        Remove model…
      </Item>
    </>
  );
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger render={<div />} className="model-context">
        <div
          ref={setNodeRef}
          {...attributes}
          {...listeners}
          role="group"
          aria-label={"Drag " + m.id}
          onMouseDown={(event) => {
            if (
              (event.target as HTMLElement).closest(
                "button:not(.model-name), input, select, a",
              )
            )
              return;
            listeners?.onMouseDown?.(event);
          }}
          onTouchStart={(event) => {
            if (
              (event.target as HTMLElement).closest(
                "button:not(.model-name), input, select, a",
              )
            )
              return;
            listeners?.onTouchStart?.(event);
          }}
          onKeyDown={(event) => {
            if (event.target === event.currentTarget)
              listeners?.onKeyDown?.(event);
          }}
          className={`deployment-row ${policy ? "policy-model" : m.loaded ? "running-model" : "available-model"} ${isDragging ? "is-dragging" : ""}`}
        >
          <input
            type="checkbox"
            aria-label={"Select " + m.id}
            checked={selected}
            onChange={onSelect}
          />
          {!policy && m.loaded && (
            <span className="service-mark">
              <ServiceIcon service={m.service} />
            </span>
          )}
          <div className="deployment-identity">
            <button
              className="model-name"
              title={m.id}
              onClick={() => onInspect(m)}
            >
              {m.id}
            </button>
            <div className="deployment-caption">
              {modelQuant(m) && (
                <span className="quant-badge">{modelQuant(m)}</span>
              )}
              <span>{m.recipe}</span>
              <CapabilityIcons model={m} />
              {policy ? (
                <span
                  className={
                    "runtime-indicator " + (m.loaded ? "is-ready" : "")
                  }
                >
                  <i />
                  {m.operation?.state ||
                    (m.loaded
                      ? m.runtime?.is_busy
                        ? "Busy"
                        : "Loaded"
                      : "Unloaded")}
                </span>
              ) : (
                <>
                  {m.service === "Chat" && m.context_length ? (
                    <span>
                      {m.context_length.toLocaleString()} total context budget
                    </span>
                  ) : null}
                  {m.size ? <span>{bytes(m.size * 1e9)}</span> : null}
                </>
              )}
            </div>
            {m.operation?.error && (
              <p className="inline-error">{m.operation.error}</p>
            )}
            {m.runtime?.settings_pending && (
              <p className="settings-pending">Saved settings need reload</p>
            )}
          </div>
          {!policy && (
            <span className={"runtime-status " + (m.loaded ? "is-ready" : "")}>
              <i />
              {m.operation?.pending ? (
                <>
                  <LoaderCircle size={12} className="spin" />
                  {m.operation.state}
                </>
              ) : m.loaded ? (
                m.runtime?.is_busy ? (
                  "Busy"
                ) : (
                  "Ready"
                )
              ) : (
                policyNames[m.policy]
              )}
            </span>
          )}
          <div className="deployment-actions">
            {!policy &&
              m.loaded &&
              ["Chat", "Transcribe", "Speak", "Images"].includes(m.service) && (
                <Button
                  variant="outline"
                  size="compact"
                  onClick={() => onPlay(m)}
                >
                  Try
                  <ArrowUpRight size={13} />
                </Button>
              )}
            {!policy && (
              <Button
                size="compact"
                variant="ghost"
                disabled={
                  m.policy === "disabled" ||
                  m.operation?.pending ||
                  (m.loaded && m.policy === "hot")
                }
                title={
                  m.loaded && m.policy === "hot"
                    ? "Change to On demand before unloading a Hot model"
                    : undefined
                }
                onClick={() => onAction(m, m.loaded ? "unload" : "load")}
              >
                {m.loaded ? (
                  "Unload"
                ) : (
                  <>
                    <Play size={12} />
                    Load
                  </>
                )}
              </Button>
            )}
            <Menu.Root>
              <Menu.Trigger
                render={
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    aria-label={"Actions for " + m.id}
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
  activity,
  policy,
  service,
  notify,
  refresh,
  onInspect,
  onRemove,
  onPlay,
  onPolicy,
}: {
  models: Model[];
  activity: ReactNode;
  policy: boolean;
  service: string;
  notify: (s: string) => void;
  refresh: () => Promise<void>;
  onInspect: (m: Model) => void;
  onRemove: (m: Model) => void;
  onPlay: (m: Model) => void;
  onPolicy: () => void;
}) {
  const [q, setQ] = useState(""),
    [sort, setSort] = useState("name"),
    [selected, setSelected] = useState<Set<string>>(new Set()),
    [limit, setLimit] = useState(60),
    [dragged, setDragged] = useState<Model>(),
    [expanded, setExpanded] = useState(false);
  useEffect(() => {
    setSelected(new Set());
    setLimit(60);
  }, [policy, service]);
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 7 } }),
    useSensor(TouchSensor, {
      activationConstraint: { delay: 220, tolerance: 6 },
    }),
    useSensor(KeyboardSensor),
  );
  const eligible = models.filter(
    (m) =>
      m.downloaded &&
      m.available !== false &&
      (service === "All services" || m.service === service),
  );
  const visible = eligible
    .filter((m) =>
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
  const running = visible.filter((m) => m.loaded || m.operation?.pending),
    unloaded = visible.filter((m) => !m.loaded && !m.operation?.pending);
  async function action(m: Model, a: string) {
    if (a === "unload" && m.policy === "hot") {
      notify("Change this model to On demand before unloading it.");
      return;
    }
    try {
      await api(
        a.startsWith("policy:") ? "juiced/policy" : "juiced/operation",
        a.startsWith("policy:")
          ? { model: m.id, policy: a.slice(7) }
          : { model: m.id, action: a },
      );
      await refresh();
      notify(
        a.startsWith("policy:")
          ? `Saved ${policyNames[a.slice(7) as keyof typeof policyNames]} policy for ${m.id}.`
          : `Requested ${a} for ${m.id}.`,
      );
    } catch (e) {
      notify((e as Error).message);
    }
  }
  function drop(e: DragEndEvent) {
    setDragged(undefined);
    if (!e.over) return;
    const m = models.find((x) => x.id === e.active.id);
    if (m)
      void action(
        m,
        String(e.over.id).startsWith("policy:")
          ? String(e.over.id)
          : e.over.id === "runtime:Loaded"
            ? "load"
            : "unload",
      );
  }
  async function bulk(a: string) {
    let ok = 0;
    const errors: string[] = [];
    for (const id of selected)
      try {
        const m = models.find((m) => m.id === id);
        if (a === "unload" && m?.policy === "hot")
          throw Error("Change Hot policy before unloading");
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
    await refresh();
    notify(
      `${ok} changes accepted.` +
        (errors.length ? " " + errors.join("; ") : ""),
    );
    setSelected(new Set());
  }
  const row = (m: Model) => (
    <ModelRow
      key={m.id}
      model={m}
      policy={policy}
      selected={selected.has(m.id)}
      onSelect={() =>
        setSelected((prev) => {
          const s = new Set(prev);
          s.has(m.id) ? s.delete(m.id) : s.add(m.id);
          return s;
        })
      }
      onAction={action}
      onInspect={onInspect}
      onRemove={onRemove}
      onPlay={onPlay}
    />
  );
  return (
    <div className={policy ? "policy-workspace" : "runtime-workspace"}>
      <header className="page-heading">
        <div>
          <h1>{policy ? "Loading policy" : "Runtime dashboard"}</h1>
          <p>
            {policy
              ? "Decide which models stay ready, load when needed, or stay out of the way."
              : "Monitor serving readiness and manage loaded models."}
          </p>
        </div>
        {!policy && (
          <Button variant="outline" size="compact" onClick={onPolicy}>
            <SlidersHorizontal size={14} />
            Edit loading policy
          </Button>
        )}
      </header>
      {!policy && (
        <div className="dashboard-overview">
          <div>
            <span>Ready models</span>
            <strong>
              {eligible.filter((m) => m.loaded && !m.runtime?.is_busy).length}
            </strong>
            <small>Serving now</small>
          </div>
          <div>
            <span>Busy models</span>
            <strong>
              {eligible.filter((m) => m.loaded && m.runtime?.is_busy).length}
            </strong>
            <small>
              {eligible.filter((m) => m.operation?.pending).length} operations
              pending
            </small>
          </div>
          <div>
            <span>Hot readiness</span>
            <strong>
              {eligible.filter((m) => m.policy === "hot" && m.loaded).length}
              <em> / {eligible.filter((m) => m.policy === "hot").length}</em>
            </strong>
            <small>Startup and recovery</small>
          </div>
          <div>
            <span>Downloaded</span>
            <strong>{models.filter((m) => m.downloaded).length}</strong>
            <small>
              {
                models.filter((m) => m.downloaded && m.policy === "disabled")
                  .length
              }{" "}
              Disabled
            </small>
          </div>
        </div>
      )}
      {!policy && activity}
      <div className="workspace-toolbar">
        <label className="search-input">
          <Search size={15} />
          <input
            aria-label="Search models"
            value={q}
            placeholder={
              policy
                ? "Find a model to change its policy…"
                : "Find a model by name, backend or capability…"
            }
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
            value=""
            onChange={(e) => {
              if (e.target.value) void bulk(e.target.value);
            }}
          >
            <option value="">Choose action…</option>
            {!policy && (
              <>
                <option value="load">Load</option>
                <option value="unload">Unload</option>
              </>
            )}
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
      <DndContext
        sensors={sensors}
        collisionDetection={(args) => {
          const hits = pointerWithin(args);
          return hits.length ? hits : rectIntersection(args);
        }}
        onDragStart={(e) =>
          setDragged(models.find((m) => m.id === e.active.id))
        }
        onDragEnd={drop}
        onDragCancel={() => setDragged(undefined)}
      >
        {policy ? (
          <>
            <div className="policy-board">
              {policies.map(({ id, title, description, Icon }) => {
                const items = visible.filter((m) => m.policy === id);
                return (
                  <DropArea
                    id={"policy:" + id}
                    className={"policy-lane policy-" + id}
                    key={id}
                  >
                    <header className="policy-lane-heading">
                      <div>
                        <Icon size={17} />
                        <h2>{title}</h2>
                        <span>{items.length}</span>
                      </div>
                      <p>{description}</p>
                    </header>
                    <div className="policy-lane-models">
                      {items.slice(0, limit).map(row)}
                      {!items.length && (
                        <div className="policy-empty">
                          <Icon size={23} />
                          <p>
                            {q ? "No matching models" : "Drop a model here"}
                          </p>
                          <small>Or change its policy from the ⋯ menu.</small>
                        </div>
                      )}
                      {items.length > limit && (
                        <Button
                          variant="ghost"
                          onClick={() => setLimit(limit + 60)}
                        >
                          Show more
                        </Button>
                      )}
                    </div>
                  </DropArea>
                );
              })}
            </div>
            <p className="page-note">
              Drag a model between groups or use its menu. Changing policy keeps
              downloaded files. On demand models can remain loaded until you
              unload them.
            </p>
          </>
        ) : (
          <>
            <DropArea id="runtime:Loaded" className="runtime-live">
              <div className="section-heading">
                <h2>Loaded</h2>
                <span>{running.length} models</span>
              </div>
              {!running.length && (
                <div className="workspace-empty">
                  <Power size={24} />
                  <h3>
                    {q ? "No loaded models match" : "No models are loaded"}
                  </h3>
                  <p>Load one from the downloaded models below.</p>
                </div>
              )}
              <div className="dashboard-services">
                {Array.from(
                  new Set((q ? visible : eligible).map((m) => m.service)),
                )
                  .sort(
                    (a, b) =>
                      ["Chat", "Transcribe", "Speak", "Images"].indexOf(a) -
                      ["Chat", "Transcribe", "Speak", "Images"].indexOf(b),
                  )
                  .map((s) => (
                    <section className="live-service" key={s}>
                      <header>
                        <ServiceIcon service={s} size={15} />
                        <h3>{serviceNames[s] || s}</h3>
                        <span>
                          {running.filter((m) => m.service === s).length}
                        </span>
                      </header>
                      {running.filter((m) => m.service === s).map(row)}
                      {!running.some((m) => m.service === s) && (
                        <div className="service-idle">
                          <Power size={17} />
                          <p>
                            {q ? "No matching model loaded" : "No model loaded"}
                          </p>
                          <small>
                            {eligible.filter((m) => m.service === s).length}{" "}
                            downloaded ·{" "}
                            {
                              eligible.filter(
                                (m) =>
                                  m.service === s && m.policy !== "disabled",
                              ).length
                            }{" "}
                            enabled
                          </small>
                          <Button
                            size="compact"
                            variant="ghost"
                            onClick={onPolicy}
                          >
                            Set loading policy
                          </Button>
                        </div>
                      )}
                    </section>
                  ))}
              </div>
            </DropArea>
            <DropArea id="runtime:Unloaded" className="runtime-shelf">
              <div className="section-heading">
                <div>
                  <h2>Unloaded models</h2>
                  <p>Enable a Disabled model before loading it.</p>
                </div>
                <Button
                  size="compact"
                  variant="outline"
                  aria-expanded={expanded || Boolean(q)}
                  onClick={() => {
                    if (q) {
                      setQ("");
                      setExpanded(false);
                    } else setExpanded(!expanded);
                  }}
                >
                  {q
                    ? "Clear search"
                    : expanded
                      ? "Hide inventory"
                      : "Show " + unloaded.length + " models"}
                </Button>
              </div>
              {(expanded || Boolean(q)) && (
                <>
                  <div className="inventory-column-head">
                    <span>Model / backend</span>
                    <span>Loading policy</span>
                    <span>Action</span>
                  </div>
                  {unloaded.slice(0, limit).map(row)}
                  {!unloaded.length && (
                    <p className="empty-state">
                      No unloaded models match this view.
                    </p>
                  )}
                  {unloaded.length > limit && (
                    <Button
                      variant="ghost"
                      onClick={() => setLimit(limit + 60)}
                    >
                      Show more
                    </Button>
                  )}
                </>
              )}
            </DropArea>
          </>
        )}
        <DragOverlay>
          {dragged && (
            <div className="model-drag-preview">
              <GripVertical size={14} />
              {dragged.id}
            </div>
          )}
        </DragOverlay>
      </DndContext>
    </div>
  );
}
