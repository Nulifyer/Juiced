import { useEffect, useId, useRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export function Button({ variant = "outline", className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "outline" | "primary" | "ghost" | "danger" }) {
  return <button type="button" className={`button ${variant} ${className}`} {...props} />;
}

export type IconName = "models" | "policy" | "library" | "backend" | "cache" | "api" | "diagnostics" | "test" | "refresh" | "close" | "menu" | "back" | "settings" | "info" | "chat" | "mic" | "layers" | "pause" | "play" | "download" | "upload" | "dashboard" | "chevron" | "cpu";
const paths: Record<IconName, ReactNode> = {
  models: <><rect x="3" y="3" width="18" height="7" rx="2" /><rect x="3" y="14" width="18" height="7" rx="2" /><path d="M7 6h.01M7 17h.01" /></>,
  policy: <><path d="M4 6h16M4 12h16M4 18h16M8 3v6M16 9v6M10 15v6" /></>,
  library: <><path d="M4 3v18M9 3v18M14 3v18M18 3l3 18" /></>,
  backend: <><rect x="5" y="5" width="14" height="14" rx="2" /><rect x="9" y="9" width="6" height="6" /><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" /></>,
  cache: <><ellipse cx="12" cy="5" rx="8" ry="3" /><path d="M4 5v14c0 4 16 4 16 0V5M4 12c0 4 16 4 16 0" /></>,
  api: <><path d="m8 6-6 6 6 6M16 6l6 6-6 6M14 3l-4 18" /></>,
  diagnostics: <><path d="M3 12h4l3-8 4 16 3-8h4" /></>,
  test: <><path d="m8 4 12 8-12 8Z" /></>,
  refresh: <><path d="M20 7a8 8 0 1 0 0 10M20 3v5h-5" /></>,
  close: <path d="m6 6 12 12M6 18 18 6" />,
  menu: <path d="M4 6h16M4 12h16M4 18h16" />,
  back: <path d="m12 4-8 8 8 8M4 12h16" />,
  settings: <><circle cx="12" cy="12" r="3" /><path d="m9 3-1 3-3 1-2 3 2 2-1 3 2 3h3l3 3 3-2 3 1 3-3-1-3 2-3-2-3-3-1-1-3Z" /></>,
  info: <><circle cx="12" cy="12" r="9" /><path d="M12 11v5M12 8h.01" /></>,
  chat: <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2Z" />,
  mic: <><rect x="9" y="2" width="6" height="12" rx="3" /><path d="M5 10a7 7 0 0 0 14 0M12 17v4" /></>,
  layers: <><path d="m12 3 9 5-9 5-9-5Z" /><path d="m3 13 9 5 9-5" /></>,
  pause: <path d="M9 5v14M15 5v14" />,
  play: <path d="m8 5 11 7-11 7Z" />,
  download: <path d="M12 3v12m-5-5 5 5 5-5M4 20h16" />,
  upload: <path d="M12 16V4m-5 5 5-5 5 5M4 20h16" />,
  dashboard: <><rect x="3" y="3" width="7" height="9" rx="1.5" /><rect x="14" y="3" width="7" height="5" rx="1.5" /><rect x="14" y="12" width="7" height="9" rx="1.5" /><rect x="3" y="16" width="7" height="5" rx="1.5" /></>,
  chevron: <path d="m6 9 6 6 6-6" />,
  cpu: <><rect x="6" y="6" width="12" height="12" rx="2" /><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4" /></>,
};
export function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}

export function HelpTip({ label, children }: { label: string; children: ReactNode }) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const place = (popup: HTMLElement) => {
    const anchor = trigger.current?.getBoundingClientRect();
    if (!anchor) return;
    const width = popup.offsetWidth, gutter = 12;
    popup.style.left = `${Math.max(gutter, Math.min(anchor.left, innerWidth - width - gutter))}px`;
    popup.style.top = `${anchor.bottom + 6}px`;
  };
  return <>
    <button ref={trigger} type="button" className="help-trigger" aria-label={`About ${label}`} popoverTarget={id}><Icon name="info" size={14} /></button>
    <div id={id} popover="auto" className="help-popup" onToggle={(event) => { if ((event as unknown as ToggleEvent).newState === "open") place(event.currentTarget); }}>{children}</div>
  </>;
}

export function Badge({ children, tone = "quiet" }: { children: ReactNode; tone?: "quiet" | "good" | "warning" | "bad" }) {
  return <span className={`badge ${tone}`}>{children}</span>;
}

export function Dialog({ title, children, onClose }: { title: string; children: ReactNode; onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  const id = title.toLowerCase().replaceAll(" ", "-");
  useEffect(() => {
    const element = ref.current!;
    element.showModal();
    return () => element.close();
  }, []);
  return <dialog ref={ref} aria-labelledby={id} onCancel={(event) => { event.preventDefault(); onClose(); }} onClick={(event) => { if (event.target === ref.current) onClose(); }}>
    <div className="dialog-heading"><h2 id={id}>{title}</h2><Button variant="ghost" aria-label="Close dialog" onClick={onClose}><Icon name="close" /></Button></div>
    {children}
  </dialog>;
}

export function Empty({ title, children }: { title: string; children: ReactNode }) {
  return <div className="empty"><h2>{title}</h2><p>{children}</p></div>;
}

export function Tabs<T extends string>({ label, value, onChange, items }: { label: string; value: T; onChange: (value: T) => void; items: readonly (readonly [T, string])[] }) {
  const select = (index: number) => { const next = items[(index + items.length) % items.length]; if (next) { onChange(next[0]); document.getElementById(`tab-${label}-${next[0]}`)?.focus(); } };
  return <div className="page-tabs" role="tablist" aria-label={label} onKeyDown={(event) => {
    const index = items.findIndex(([id]) => id === value);
    if (event.key === "ArrowRight") { event.preventDefault(); select(index + 1); }
    if (event.key === "ArrowLeft") { event.preventDefault(); select(index - 1); }
  }}>
    {items.map(([id, name]) => <button key={id} id={`tab-${label}-${id}`} role="tab" aria-selected={value === id} tabIndex={value === id ? 0 : -1} onClick={() => onChange(id)}>{name}</button>)}
  </div>;
}

export function Menu({ label, trigger, children }: { label: string; trigger: ReactNode; children: ReactNode }) {
  const id = useId();
  const button = useRef<HTMLButtonElement>(null);
  const place = (popup: HTMLElement) => {
    const anchor = button.current?.getBoundingClientRect();
    if (!anchor) return;
    popup.style.top = `${anchor.bottom + 6}px`;
    popup.style.left = `${Math.max(12, Math.min(anchor.right - popup.offsetWidth, innerWidth - popup.offsetWidth - 12))}px`;
  };
  return <>
    <button ref={button} type="button" className="menu-trigger" aria-label={label} aria-haspopup="menu" popoverTarget={id}>{trigger}</button>
    <div id={id} popover="auto" className="menu-popup" role="menu" onToggle={(event) => { if ((event as unknown as ToggleEvent).newState === "open") place(event.currentTarget); }} onClick={(event) => { if ((event.target as HTMLElement).closest("button")) (event.currentTarget as HTMLElement).hidePopover(); }}>{children}</div>
  </>;
}
