import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { ChatMarkdown } from "./ChatMarkdown";
import { MAX_IMAGES, imageType, readImage, type Attachment } from "./chatImages";
import { Button, Icon } from "./ui";
import "./chat.css";

const seconds = (ms: number) => Math.max(1, Math.round(ms / 1000));

/** Tracks how long a reply spent reasoning before its answer or tool calls began. */
export function thinkingClock() {
  let started: number | null = null, ended: number | null = null;
  return {
    observe(reasoning: string, answered: boolean) {
      const now = performance.now();
      if (reasoning && started === null) started = now;
      if (answered && started !== null && ended === null) ended = now;
      return { since: started === null ? undefined : Date.now() - (now - started), thoughtMs: started !== null && ended !== null ? ended - started : undefined };
    },
    finish() { const now = performance.now(); if (started !== null && ended === null) ended = now; return started !== null && ended !== null ? ended - started : undefined; },
  };
}

function useElapsed(since: number | undefined, active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active || since === undefined) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active, since]);
  return since === undefined ? 0 : Math.max(0, now - since);
}

/** A one-line reasoning summary that expands to the model's reasoning text. */
export function Reasoning({ text, active, since, ms }: { text: string; active: boolean; since?: number; ms?: number }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const elapsed = useElapsed(since, active);
  if (!text) return null;
  const label = active ? `Thinking${elapsed >= 1000 ? ` ${seconds(elapsed)}s` : "…"}` : ms !== undefined ? `Thought for ${seconds(ms)}s` : "Thought";
  return <div className={`reasoning ${active ? "active" : ""}`}>
    <button type="button" className="reasoning-toggle" aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}><Icon name="chevron" size={13} /><span>{label}</span></button>
    {open && <div id={id} className="reasoning-text"><ChatMarkdown text={text} streaming={active} /></div>}
  </div>;
}

export function AssistantBody({ text, reasoning, streaming, since, thoughtMs, empty = "Thinking…" }: { text: string; reasoning: string; streaming: boolean; since?: number; thoughtMs?: number; empty?: string }) {
  return <>
    <Reasoning text={reasoning} active={streaming && !text} since={since} ms={thoughtMs} />
    {text ? <ChatMarkdown text={text} streaming={streaming} /> : streaming && !reasoning && <p className="muted typing">{empty}</p>}
  </>;
}

export function UserBody({ text, images }: { text: string; images?: string[] }) {
  return <>
    {images && images.length > 0 && <div className="sent-images">{images.map((url, index) => <img key={index} src={url} alt={`Image ${index + 1}`} />)}</div>}
    {text && <p className="user-text">{text}</p>}
  </>;
}

/** Images waiting to be sent with the next message. */
export function useAttachments(enabled: boolean) {
  const [list, setList] = useState<Attachment[]>([]);
  const [error, setError] = useState("");
  const [reading, setReading] = useState(0);
  const current = useRef<Attachment[]>([]);
  const update = useCallback((next: Attachment[]) => { current.current = next; setList(next); }, []);
  useEffect(() => { if (!enabled) update([]); }, [enabled, update]);
  const add = useCallback(async (files: File[]) => {
    const images = files.filter((file) => imageType(file));
    if (images.length === 0) return false;
    if (!enabled) { setError("This model does not accept images."); return true; }
    setError("");
    setReading((count) => count + 1);
    try {
      for (const file of images) {
        if (current.current.length >= MAX_IMAGES) { setError(`Send up to ${MAX_IMAGES} images at a time.`); break; }
        try { const image = await readImage(file); if (current.current.length < MAX_IMAGES) update([...current.current, image]); }
        catch (failure) { setError(failure instanceof Error ? failure.message : "The image could not be read."); }
      }
    } finally { setReading((count) => count - 1); }
    return true;
  }, [enabled, update]);
  return { list, error, reading: reading > 0, add, setError, remove: (id: string) => update(current.current.filter((image) => image.id !== id)), clear: () => { update([]); setError(""); } };
}
export type Attachments = ReturnType<typeof useAttachments>;

/** Message box with Send/Stop and, for vision models, image paste, drop and pick. */
export function Composer({ label, placeholder, value, onChange, onSubmit, busy, onStop, canSend, attachments, vision, rows = 2, inputRef, submitLabel = "Send", children }: {
  label: string; placeholder: string; value: string; onChange: (value: string) => void; onSubmit: () => void; busy: boolean; onStop: () => void; canSend: boolean;
  attachments?: Attachments; vision?: boolean; rows?: number; inputRef?: React.Ref<HTMLTextAreaElement>; submitLabel?: string; children?: ReactNode;
}) {
  const picker = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const files = (list: FileList | null | undefined) => Array.from(list ?? []);
  const drop = attachments ? {
    onDragOver: (event: React.DragEvent) => { if (Array.from(event.dataTransfer.items).some((item) => item.kind === "file")) { event.preventDefault(); setDragging(true); } },
    onDragLeave: (event: React.DragEvent) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false); },
    onDrop: (event: React.DragEvent) => { setDragging(false); const dropped = files(event.dataTransfer.files); if (dropped.length) { event.preventDefault(); void attachments.add(dropped); } },
  } : {};
  return <form className={`composer ${dragging && vision ? "dragging" : ""}`} onSubmit={(event) => { event.preventDefault(); onSubmit(); }} {...drop}>
    {children}
    {attachments && attachments.list.length > 0 && <ul className="attachment-strip" aria-label="Images to send">{attachments.list.map((image) => <li key={image.id}>
      <img src={image.url} alt={image.name} />
      <button type="button" aria-label={`Remove ${image.name}`} onClick={() => attachments.remove(image.id)}><Icon name="close" size={12} /></button>
    </li>)}</ul>}
    {attachments?.error && <p className="composer-error" role="alert">{attachments.error}</p>}
    <div className="composer-row">
      {attachments && vision && <>
        <Button variant="ghost" className="attach" aria-label="Add images" title="Add images" disabled={busy || attachments.reading} onClick={() => picker.current?.click()}><Icon name="image" size={16} /></Button>
        <input ref={picker} type="file" accept="image/*" multiple hidden aria-label="Image files" onChange={(event) => { void attachments.add(files(event.target.files)); event.target.value = ""; }} />
      </>}
      <textarea ref={inputRef} aria-label={label} rows={rows} value={value} placeholder={placeholder} onChange={(event) => onChange(event.target.value)}
        onPaste={attachments ? (event) => { const pasted = files(event.clipboardData.files); if (pasted.some((file) => imageType(file))) { event.preventDefault(); void attachments.add(pasted); } } : undefined}
        onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); onSubmit(); } }} />
      {busy ? <Button key="stop" variant="danger" onClick={onStop}>Stop</Button> : <Button key="send" variant="primary" type="submit" disabled={!canSend || attachments?.reading}>{submitLabel}</Button>}
    </div>
  </form>;
}
