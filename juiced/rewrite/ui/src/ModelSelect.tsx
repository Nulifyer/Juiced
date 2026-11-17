import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { bytes, type Model } from "./domain";
import { modelState, modelType, pickerView, typeLabel, type ModelType } from "./modelPicker";
import { Icon } from "./ui";
import "./model-select.css";

type Option = { key: string; value?: string; label: string; model?: Model; note?: string };
const MORE = "more";
const PAGE = 8;

const fine = () => matchMedia("(pointer: fine)").matches;

function fact(model: Model): { text: string; tone?: "bad" | "warning" } {
  if (model.state === "failed") return { text: "Failed", tone: "bad" };
  if (model.downloaded === false || model.state === "missing-weights") return { text: "Files missing", tone: "warning" };
  if (model.state === "loading") return { text: "Loading" };
  return { text: model.resident_bytes ? bytes(model.resident_bytes) : "" };
}

/**
 * The console's one model picker: a button that opens a searchable list grouped as Loaded, Enabled
 * and Disabled. `leading` adds choices that are not models, such as `["", "All sources"]`.
 */
export function ModelSelect({ models, value, onChange, placeholder = "Choose a model…", leading = [], disabledModels = "toggle", id, disabled, ...naming }: { models: Model[]; value: string; onChange: (id: string) => void; placeholder?: string; leading?: [string, string][]; disabledModels?: "toggle" | "never"; id?: string; disabled?: boolean; "aria-label"?: string; "aria-labelledby"?: string }) {
  const popupId = useId(), listId = useId();
  const trigger = useRef<HTMLButtonElement>(null), popup = useRef<HTMLDivElement>(null), input = useRef<HTMLInputElement>(null), list = useRef<HTMLDivElement>(null);
  const seed = useRef(""), above = useRef(false), reveal = useRef(false);
  const [open, setOpen] = useState(false);
  const [opened, setOpened] = useState(0);
  const [query, setQuery] = useState("");
  const [type, setType] = useState<ModelType | null>(null);
  const [showDisabled, setShowDisabled] = useState(false);
  const [active, setActive] = useState<string | null>(null);

  const view = useMemo(() => open ? pickerView(models, { query, type, showDisabled, selected: value, disabledModels }) : null, [open, models, query, type, showDisabled, value, disabledModels]);
  const lead = leading.find(([option]) => option === value);
  const known = lead !== undefined || value === "" || models.some((model) => model.id === value);
  const options = useMemo(() => {
    if (!view) return [];
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const plain = (label: string) => type === null && words.every((word) => label.toLowerCase().includes(word));
    const rows: Option[] = leading.filter(([, label]) => plain(label)).map(([option, label]) => ({ key: `v:${option}`, value: option, label }));
    if (!known && plain(value)) rows.push({ key: `v:${value}`, value, label: value, note: "Not in this list" });
    for (const group of view.groups) for (const model of group.models) rows.push({ key: `v:${model.id}`, value: model.id, label: model.id, model });
    if (view.hidden > 0) rows.push({ key: MORE, label: `Show ${view.hidden} disabled` });
    return rows;
  }, [view, leading, query, type, known, value]);
  const positions = useMemo(() => new Map(options.map((option, position) => [option.key, position])), [options]);
  const fallback = (query.trim() ? options.find((option) => option.model) : options.find((option) => option.value === value)) ?? options[0];
  const current = (active === null ? undefined : options[positions.get(active) ?? -1]) ?? fallback;
  const index = current ? positions.get(current.key)! : -1;
  const typed = view ? view.types.length > 1 && view.total > 8 : false;
  const columns = view ? view.types.length > 1 : false;

  function place() {
    const anchor = trigger.current?.getBoundingClientRect(), element = popup.current;
    if (!anchor || !element) return;
    const viewport = window.visualViewport, gutter = 8, gap = 4;
    const bottomEdge = (viewport?.offsetTop ?? 0) + (viewport?.height ?? innerHeight);
    const pageWidth = document.documentElement.clientWidth;
    const width = Math.min(pageWidth - gutter * 2, Math.max(anchor.width, 520));
    const below = bottomEdge - anchor.bottom - gap - gutter, over = anchor.top - gap - gutter;
    if (!element.dataset.ready) above.current = below < 260 && over > below;
    element.style.width = `${width}px`;
    element.style.left = `${Math.max(gutter, Math.min(anchor.left, pageWidth - width - gutter))}px`;
    element.style.maxHeight = `${Math.max(160, Math.min(440, above.current ? over : below))}px`;
    element.style.top = above.current ? "auto" : `${anchor.bottom + gap}px`;
    element.style.bottom = above.current ? `${innerHeight - anchor.top + gap}px` : "auto";
    element.dataset.ready = "true";
  }

  useLayoutEffect(() => {
    if (!open) return;
    place();
    // A touch keyboard would cover half the list, so touch users tap the search box when they want it.
    if (seed.current || fine()) input.current?.focus();
    reveal.current = true;
    const viewport = window.visualViewport;
    addEventListener("resize", place); addEventListener("scroll", place, true); viewport?.addEventListener("resize", place);
    return () => { removeEventListener("resize", place); removeEventListener("scroll", place, true); viewport?.removeEventListener("resize", place); };
  }, [open, opened]);

  useEffect(() => {
    if (!reveal.current || index < 0) return;
    reveal.current = false;
    document.getElementById(`${listId}-${index}`)?.scrollIntoView({ block: "nearest" });
  });

  function close(refocus: boolean) {
    if (popup.current?.matches(":popover-open")) popup.current.hidePopover();
    if (refocus) trigger.current?.focus();
  }
  function choose(option: Option | undefined) {
    if (!option) return;
    if (option.key === MORE) {
      const next = pickerView(models, { query, type, showDisabled: true, selected: value, disabledModels }).groups.find((group) => group.state === "disabled")?.models.find((model) => model.id !== value);
      setShowDisabled(true); setActive(next ? `v:${next.id}` : null); reveal.current = true;
      return;
    }
    close(true);
    if (option.value !== undefined && option.value !== value) onChange(option.value);
  }
  function move(event: KeyboardEvent, next: number) {
    event.preventDefault();
    const target = options[Math.max(0, Math.min(options.length - 1, next))];
    if (target) { setActive(target.key); reveal.current = true; }
  }
  function keys(event: KeyboardEvent) {
    if (event.key === "ArrowDown") move(event, index + 1);
    else if (event.key === "ArrowUp") move(event, index - 1);
    else if (event.key === "Home") move(event, 0);
    else if (event.key === "End") move(event, options.length - 1);
    else if (event.key === "PageDown") move(event, index + PAGE);
    else if (event.key === "PageUp") move(event, index - PAGE);
    else if (event.key === "Enter") { event.preventDefault(); choose(current); }
    else if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(true); }
  }
  function triggerKeys(event: KeyboardEvent) {
    const letter = event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey;
    // The popover state leads React's: toggle events arrive a task later, and keys typed in between still count.
    const shown = popup.current?.matches(":popover-open") === true;
    if (shown && open) {
      if (letter) { event.preventDefault(); setQuery(query + event.key); setActive(null); input.current?.focus(); }
      else keys(event);
    } else if (letter || event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (shown) { if (letter) seed.current += event.key; return; }
      seed.current = letter ? event.key : "";
      popup.current?.showPopover();
    }
  }

  function row(option: Option) {
    const position = positions.get(option.key), model = option.model;
    const detail = model ? fact(model) : undefined;
    return <div key={option.key} id={`${listId}-${position}`} role="option" className="model-option" aria-selected={option.value === value} data-active={option === current || undefined} data-value={option.value} data-more={option.key === MORE || undefined}
      onPointerDown={(event) => { if (event.pointerType === "mouse") event.preventDefault(); }} onPointerMove={(event) => { if (event.pointerType === "mouse" && option.key !== current?.key) setActive(option.key); }} onClick={() => choose(option)}>
      <span className="state-mark" data-state={model ? modelState(model) : undefined} aria-hidden="true" />
      <span className="model-option-id">{option.label}</span>
      {model && columns && <span className="model-option-type">{typeLabel(modelType(model))}</span>}
      {(option.note || detail?.text) && <span className="model-option-fact" data-tone={detail?.tone}>{option.note ?? detail!.text}</span>}
      {option.value === value && <span className="model-option-check" aria-hidden="true"><Icon name="check" size={14} /></span>}
    </div>;
  }

  const label = lead?.[1] ?? value;
  return <>
    <button ref={trigger} id={id} type="button" className="model-select" role="combobox" aria-haspopup="dialog" aria-expanded={open} aria-controls={popupId} disabled={disabled} popoverTarget={popupId} title={label || undefined} onKeyDown={triggerKeys} onBlur={(event) => { const next = event.relatedTarget as Node | null; if (open && next && !popup.current?.contains(next)) close(false); }} {...naming}>
      <span className="model-select-value" data-empty={label ? undefined : true}>{label || placeholder}</span><Icon name="chevron" size={14} />
    </button>
    <div ref={popup} id={popupId} popover="auto" className="model-popup" role="dialog" aria-label="Choices"
      onToggle={(event) => {
        const opening = (event as unknown as ToggleEvent).newState === "open";
        if (opening) { setQuery(seed.current); setType(null); setShowDisabled(false); setActive(null); setOpened((count) => count + 1); } else { seed.current = ""; delete event.currentTarget.dataset.ready; }
        setOpen(opening);
      }}
      onBlur={(event) => { const next = event.relatedTarget as Node | null; if (next && next !== trigger.current && !event.currentTarget.contains(next)) close(false); }}>
      {view && <>
        <div className="model-popup-search">
          <input ref={input} role="combobox" aria-label="Search" aria-expanded="true" aria-controls={listId} aria-autocomplete="list" aria-activedescendant={index >= 0 ? `${listId}-${index}` : undefined} placeholder="Search by name or type" autoComplete="off" spellCheck={false} enterKeyHint="search" value={query} onChange={(event) => { setQuery(event.target.value); setActive(null); list.current?.scrollTo({ top: 0 }); }} onKeyDown={keys} />
          <span className="model-popup-count" role="status">{view.shown} of {view.total}</span>
        </div>
        {typed && <div className="chip-filter model-popup-types" role="group" aria-label="Model type">
          {view.types.map((item) => <button key={item.type} type="button" aria-pressed={type === item.type} onClick={() => { setType(type === item.type ? null : item.type); setActive(null); if (fine()) input.current?.focus(); }}>{item.label}<span>{item.count}</span></button>)}
        </div>}
        <div ref={list} id={listId} className="model-popup-list" role="listbox" aria-label="Results" data-columns={columns || undefined}>
          {options.length === 0 && <p className="model-popup-empty">{view.total === 0 ? "No models to choose from." : query.trim() ? `No model matches "${query.trim()}".` : `No ${typeLabel(type).toLowerCase()} models.`}</p>}
          {options.filter((option) => !option.model && option.key !== MORE).map(row)}
          {view.groups.map((group) => <div key={group.state} role="group" aria-labelledby={`${listId}-${group.state}`}>
            <div id={`${listId}-${group.state}`} className="model-group-label">{group.label}<span>{group.models.length}</span></div>
            {group.models.map((model) => row(options[positions.get(`v:${model.id}`)!]!))}
          </div>)}
          {options.filter((option) => option.key === MORE).map(row)}
        </div>
      </>}
    </div>
  </>;
}
