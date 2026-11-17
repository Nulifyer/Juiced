import type { Ref } from "react";
import { bytes, policyNames, type Policy } from "./domain";
import { Button } from "./ui";

export type QuantTone = "good" | "warning" | "bad" | "quiet";
export type QuantItem = {
  key: string; label: string; note?: string | null; bytes: number | null; state: string; title?: string;
  fit?: { tone: QuantTone; label: string }; current?: boolean; suggested?: boolean; progress?: number | null;
  action?: { label: string; name: string; run: () => void };
};

/** The words every quant list uses for the same state. */
export const quantWords = {
  current: "This model",
  unadded: "Not added",
  parts: (have: number, parts: number) => `${have} of ${parts} parts`,
  downloading: (percent: number) => `Downloading ${Math.round(percent)}%`,
};

/** A quant that is added as a model shows that model's loading policy. */
export function policyWord(policy: string | null | undefined, models = 1): string {
  const word = policy && Object.hasOwn(policyNames, policy) ? policyNames[policy as Policy] : "Added";
  return models > 1 ? `${word} +${models - 1}` : word;
}

function Cells({ item }: { item: QuantItem }) {
  return <>
    <span className="quant-name"><strong>{item.fit && <span className={`status-dot ${item.fit.tone}`} title={item.fit.label} />}{item.label}{item.suggested && <span className="quant-star" aria-label="Suggested">★</span>}</strong>{item.note && <span className="muted break-word">{item.note}</span>}</span>
    <span className="quant-state muted">{item.state}</span>
    <span className="muted numeric">{item.bytes === null ? "" : bytes(item.bytes)}</span>
  </>;
}

/**
 * One row per quant: name, state, size and one action. With `onSelect` the rows are options
 * and the caller acts on the selected one.
 */
export function QuantList({ label, items, selected, onSelect, listRef }: { label: string; items: QuantItem[]; selected?: string; onSelect?: (key: string) => void; listRef?: Ref<HTMLDivElement> }) {
  if (onSelect) return <div ref={listRef} className="quant-rows selectable" role="listbox" aria-label={label}>{items.map((item) => <button key={item.key} role="option" aria-selected={item.key === selected} className="quant-row" title={item.title} onClick={() => onSelect(item.key)}>
    <Cells item={item} />
    {typeof item.progress === "number" && <span className="quant-progress" style={{ width: `${item.progress}%` }} />}
  </button>)}</div>;
  return <ul className="quant-rows" aria-label={label}>{items.map((item) => <li key={item.key} className={`quant-row ${item.current ? "current" : ""}`} title={item.title}>
    <Cells item={item} />
    <span className="quant-action">{item.action && <Button className="row-action" aria-label={item.action.name} onClick={item.action.run}>{item.action.label}</Button>}</span>
    {typeof item.progress === "number" && <span className="quant-progress" style={{ width: `${item.progress}%` }} />}
  </li>)}</ul>;
}
