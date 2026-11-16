import { useEffect, useState } from "react";

export type ToastAction = { label: string; run: () => void };
export type Toast = { id: number; tone: "success" | "error"; text: string; action?: ToastAction };
let next = 1;
let items: Toast[] = [];
const listeners = new Set<(items: Toast[]) => void>();
const emit = () => listeners.forEach((listener) => listener(items));

export function notify(text: string, tone: Toast["tone"] = "success", action?: ToastAction) {
  if (!text) return;
  const id = next++;
  items = [...items.filter((item) => item.text !== text && item.tone !== "success").slice(-3), { id, tone, text, action }];
  emit();
  if (tone === "success") setTimeout(() => dismiss(id), action ? 10000 : 5000);
}
export function dismiss(id: number) { items = items.filter((item) => item.id !== id); emit(); }
export function clearToasts() { items = []; emit(); }
export function useToasts() {
  const [state, setState] = useState(items);
  useEffect(() => { listeners.add(setState); return () => { listeners.delete(setState); }; }, []);
  return state;
}
