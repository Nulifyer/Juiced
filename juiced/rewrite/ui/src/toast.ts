import { useEffect, useState } from "react";

export type Toast = { id: number; tone: "success" | "error"; text: string };
let next = 1;
let items: Toast[] = [];
const listeners = new Set<(items: Toast[]) => void>();
const emit = () => listeners.forEach((listener) => listener(items));

export function notify(text: string, tone: Toast["tone"] = "success") {
  if (!text) return;
  const id = next++;
  items = [...items.filter((item) => item.text !== text && item.tone !== "success").slice(-3), { id, tone, text }];
  emit();
  if (tone === "success") setTimeout(() => dismiss(id), 5000);
}
export function dismiss(id: number) { items = items.filter((item) => item.id !== id); emit(); }
export function clearToasts() { items = []; emit(); }
export function useToasts() {
  const [state, setState] = useState(items);
  useEffect(() => { listeners.add(setState); return () => { listeners.delete(setState); }; }, []);
  return state;
}
