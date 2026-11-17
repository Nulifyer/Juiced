import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { wordsMatch } from "./search";
import { Icon } from "./ui";

export { wordsMatch };
import "./model-select.css";

/** One choice. `mark` draws the state dot (loaded, enabled, disabled); `kind` and `fact` are the quiet columns. */
export type PickItem = { value: string; label: string; mark?: string; kind?: string; fact?: string; tone?: "bad" | "warning" };
export type PickGroup = { key: string; label: string; items: PickItem[] };
export type PickView = {
  groups: PickGroup[];
  /** Items listed, out of `total` the picker could offer. */
  shown: number;
  total: number;
  /** Items left out until asked for; `more` names them, as in "Show 12 disabled". */
  hidden: number;
  more?: string;
  chips?: { key: string; label: string; count: number }[];
  /** Reserve a column for `kind` so facts line up. */
  columns?: boolean;
};
export type PickQuery = { query: string; chip: string | null; expanded: boolean };

type Option = { key: string; value?: string; label: string; item?: PickItem; note?: string };
const MORE = "more";
const PAGE = 8;

const fine = () => matchMedia("(pointer: fine)").matches;


/**
 * The console's one searchable picker: a button that opens a filtered list of grouped items.
 * `view` does the filtering so each kind of list keeps its own search words and order.
 * `leading` adds fixed choices, such as `["", "All sources"]`.
 */
export function Picker({ value, onChange, view: build, placeholder, noun: [one, many], searchHint = "Search", chipsLabel = "Type", leading = [], display, id, disabled, ...naming }: {
  value: string; onChange: (value: string) => void; view: (query: PickQuery) => PickView; placeholder: string;
  /** Singular and plural, as in "No voice matches" and "No voices to choose from." */
  noun: [string, string]; searchHint?: string; chipsLabel?: string; leading?: [string, string][]; display?: (value: string) => string | undefined;
  id?: string; disabled?: boolean; "aria-label"?: string; "aria-labelledby"?: string;
}) {
  const popupId = useId(), listId = useId();
  const trigger = useRef<HTMLButtonElement>(null), popup = useRef<HTMLDivElement>(null), input = useRef<HTMLInputElement>(null), list = useRef<HTMLDivElement>(null);
  const seed = useRef(""), above = useRef(false), reveal = useRef(false);
  const [open, setOpen] = useState(false);
  const [opened, setOpened] = useState(0);
  const [query, setQuery] = useState("");
  const [chip, setChip] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [active, setActive] = useState<string | null>(null);

  const view = useMemo(() => open ? build({ query, chip, expanded }) : null, [open, build, query, chip, expanded]);
  const lead = leading.find(([option]) => option === value);
  const options = useMemo(() => {
    if (!view) return [];
    const plain = (label: string) => chip === null && wordsMatch(label, query);
    const rows: Option[] = leading.filter(([, label]) => plain(label)).map(([option, label]) => ({ key: `v:${option}`, value: option, label }));
    const listed = view.groups.some((group) => group.items.some((item) => item.value === value));
    if (lead === undefined && value !== "" && !listed && !build({ query: "", chip: null, expanded: true }).groups.some((group) => group.items.some((item) => item.value === value)) && plain(value)) rows.push({ key: `v:${value}`, value, label: display?.(value) ?? value, note: "Not in this list" });
    for (const group of view.groups) for (const item of group.items) rows.push({ key: `v:${item.value}`, value: item.value, label: item.label, item });
    if (view.hidden > 0) rows.push({ key: MORE, label: view.more ?? `Show ${view.hidden} more` });
    return rows;
  }, [view, build, leading, lead, query, chip, value, display]);
  const positions = useMemo(() => new Map(options.map((option, position) => [option.key, position])), [options]);
  const fallback = (query.trim() ? options.find((option) => option.item) : options.find((option) => option.value === value)) ?? options[0];
  const current = (active === null ? undefined : options[positions.get(active) ?? -1]) ?? fallback;
  const index = current ? positions.get(current.key)! : -1;
  const chipped = view ? (view.chips?.length ?? 0) > 1 && view.total > PAGE : false;

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
      const before = new Set(options.map((row) => row.value));
      const next = build({ query, chip, expanded: true }).groups.flatMap((group) => group.items).find((item) => !before.has(item.value));
      setExpanded(true); setActive(next ? `v:${next.value}` : null); reveal.current = true;
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
    // Once there is text, Home and End move the caret, as in any text field.
    else if (event.key === "Home" && query === "") move(event, 0);
    else if (event.key === "End" && query === "") move(event, options.length - 1);
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
    const position = positions.get(option.key), item = option.item;
    const fact = option.note ?? item?.fact;
    return <div key={option.key} id={`${listId}-${position}`} role="option" className="model-option" aria-selected={option.value === value} data-active={option === current || undefined} data-value={option.value} data-more={option.key === MORE || undefined}
      onPointerDown={(event) => { if (event.pointerType === "mouse") event.preventDefault(); }} onPointerMove={(event) => { if (event.pointerType === "mouse" && option.key !== current?.key) setActive(option.key); }} onClick={() => choose(option)}>
      <span className="state-mark" data-state={item?.mark} aria-hidden="true" />
      <span className="model-option-id">{option.label}</span>
      {item && view?.columns && <span className="model-option-type">{item.kind}</span>}
      {fact && <span className="model-option-fact" data-tone={item?.tone}>{fact}</span>}
      {option.value === value && <span className="model-option-check" aria-hidden="true"><Icon name="check" size={14} /></span>}
    </div>;
  }

  const label = lead?.[1] ?? (value ? display?.(value) ?? value : "");
  const chipLabel = view?.chips?.find((item) => item.key === chip)?.label.toLowerCase();
  return <>
    <button ref={trigger} id={id} type="button" className="model-select" role="combobox" aria-haspopup="dialog" aria-expanded={open} aria-controls={popupId} disabled={disabled} popoverTarget={popupId} title={label || undefined} onKeyDown={triggerKeys} onBlur={(event) => { const next = event.relatedTarget as Node | null; if (open && next && !popup.current?.contains(next)) close(false); }} {...naming}>
      <span className="model-select-value" data-empty={label ? undefined : true}>{label || placeholder}</span><Icon name="chevron" size={14} />
    </button>
    <div ref={popup} id={popupId} popover="auto" className="model-popup" role="dialog" aria-label="Choices"
      onToggle={(event) => {
        const opening = (event as unknown as ToggleEvent).newState === "open";
        if (opening) { setQuery(seed.current); setChip(null); setExpanded(false); setActive(null); setOpened((count) => count + 1); } else { seed.current = ""; delete event.currentTarget.dataset.ready; }
        setOpen(opening);
      }}
      onBlur={(event) => { const next = event.relatedTarget as Node | null; if (next && next !== trigger.current && !event.currentTarget.contains(next)) close(false); }}>
      {view && <>
        <div className="model-popup-search">
          <input ref={input} role="combobox" aria-label="Search" aria-expanded="true" aria-controls={listId} aria-autocomplete="list" aria-activedescendant={index >= 0 ? `${listId}-${index}` : undefined} placeholder={searchHint} autoComplete="off" spellCheck={false} enterKeyHint="search" value={query} onChange={(event) => { setQuery(event.target.value); setActive(null); list.current?.scrollTo({ top: 0 }); }} onKeyDown={keys} />
          <span className="model-popup-count" role="status">{view.shown} of {view.total}</span>
        </div>
        {chipped && <div className="chip-filter model-popup-types" role="group" aria-label={chipsLabel}>
          {view.chips!.map((item) => <button key={item.key} type="button" aria-pressed={chip === item.key} onClick={() => { setChip(chip === item.key ? null : item.key); setActive(null); if (fine()) input.current?.focus(); }}>{item.label}<span>{item.count}</span></button>)}
        </div>}
        <div ref={list} id={listId} className="model-popup-list" role="listbox" aria-label="Results" data-columns={view.columns || undefined}>
          {options.length === 0 && <p className="model-popup-empty">{view.total === 0 ? `No ${many} to choose from.` : query.trim() ? `No ${one} matches "${query.trim()}".` : `No ${chipLabel ? `${chipLabel} ` : ""}${many}.`}</p>}
          {options.filter((option) => !option.item && option.key !== MORE).map(row)}
          {view.groups.map((group, place) => group.label
            ? <div key={group.key} role="group" aria-labelledby={`${listId}-g${place}`}>
              <div id={`${listId}-g${place}`} className="model-group-label">{group.label}<span>{group.items.length}</span></div>
              {group.items.map((item) => row(options[positions.get(`v:${item.value}`)!]!))}
            </div>
            : <div key={group.key} role="group" aria-label={many}>{group.items.map((item) => row(options[positions.get(`v:${item.value}`)!]!))}</div>)}
          {options.filter((option) => option.key === MORE).map(row)}
        </div>
      </>}
    </div>
  </>;
}
