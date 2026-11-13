import { isValidElement, memo, useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { splitMarkdownBlocks } from "./markdownBlocks";
import { Icon } from "./ui";

async function copyText(text: string): Promise<boolean> {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* Clipboard access needs a secure context and permission. */ }
  const field = document.createElement("textarea");
  field.value = text; field.setAttribute("readonly", ""); field.style.position = "fixed"; field.style.opacity = "0";
  document.body.append(field); field.select();
  try { return document.execCommand("copy"); } catch { return false; } finally { field.remove(); }
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return <button type="button" className="copy-code" aria-label={state === "copied" ? "Copied" : label} title={label} onClick={async () => {
    setState(await copyText(text) ? "copied" : "failed");
    clearTimeout(timer.current); timer.current = setTimeout(() => setState("idle"), 1400);
  }}><Icon name={state === "copied" ? "check" : "copy"} size={13} /><span>{state === "copied" ? "Copied" : state === "failed" ? "Copy failed" : "Copy"}</span></button>;
}

function plain(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(plain).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return plain(node.props.children);
  return "";
}

function CodeBlock({ children }: ComponentProps<"pre">) {
  const code = isValidElement<{ className?: string; children?: ReactNode }>(children) ? children : null;
  const language = /language-([\w+#.-]+)/.exec(code?.props.className ?? "")?.[1] ?? "";
  const text = plain(code?.props.children ?? children).replace(/\n$/, "");
  return <div className="md-code" data-language={language || undefined}>
    <div className="md-code-head"><span>{language || "text"}</span><CopyButton text={text} label="Copy code" /></div>
    <pre><code>{text}</code></pre>
  </div>;
}

const safeHref = (href: string | undefined) => href && /^(https?:|mailto:)/i.test(href) ? href : undefined;

const components: Components = {
  pre: CodeBlock,
  a: ({ href, children }) => { const target = safeHref(href); return target ? <a href={target} target="_blank" rel="noreferrer noopener">{children}</a> : <span>{children}</span>; },
  // Remote images in model output are links, so a reply cannot make the browser fetch them.
  img: ({ src, alt }) => { const target = safeHref(typeof src === "string" ? src : undefined); return target ? <a href={target} target="_blank" rel="noreferrer noopener">{alt || target}</a> : <span>{alt}</span>; },
  table: ({ children }) => <div className="md-table"><table>{children}</table></div>,
  input: ({ checked, type }) => type === "checkbox" ? <input type="checkbox" checked={!!checked} disabled readOnly /> : null,
};
const plugins = [remarkGfm];

const Block = memo(function Block({ text }: { text: string }) {
  return <Markdown remarkPlugins={plugins} components={components} skipHtml>{text}</Markdown>;
});

/** Model output as GitHub-flavoured Markdown. While streaming only the last block re-renders. */
export function ChatMarkdown({ text, streaming = false, className = "" }: { text: string; streaming?: boolean; className?: string }) {
  const blocks = (streaming ? splitMarkdownBlocks(text) : null) ?? [text];
  return <div className={`markdown ${className}`} data-streaming={streaming || undefined}>{blocks.map((block, index) => <Block key={index} text={block} />)}</div>;
}
