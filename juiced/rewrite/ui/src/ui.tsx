import { useEffect, useRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export function Button({ variant = "outline", className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "outline" | "primary" | "ghost" | "danger" }) {
  return <button type="button" className={`button ${variant} ${className}`} {...props} />;
}

export type IconName = "models" | "policy" | "library" | "backend" | "cache" | "api" | "diagnostics" | "test" | "refresh" | "close" | "menu" | "back" | "settings";
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
};
export function Icon({ name }: { name: IconName }) {
  return <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
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
