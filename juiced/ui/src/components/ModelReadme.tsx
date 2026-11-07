import { useEffect, useState } from "react";
import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
import "./ModelEffective.css";

export function ModelReadme({ repository }: { repository: string }) {
  const [content, setContent] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [retry, setRetry] = useState(0);
  const valid = /^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/.test(repository);
  useEffect(() => {
    setContent(""); setError("");
    if (!valid) return;
    const controller = new AbortController();
    setLoading(true);
    void fetch(`https://huggingface.co/${repository}/raw/main/README.md`, { signal: AbortSignal.any([controller.signal, AbortSignal.timeout(12000)]) })
      .then(async (response) => {
        if (!response.ok) throw Error(response.status === 404 ? "This repository does not publish a README." : `The publisher returned HTTP ${response.status}.`);
        const text = await response.text();
        if (text.length > 500000) throw Error("The model card is too large to preview. Open the publisher repository instead.");
        setContent(text.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, ""));
      })
      .catch((error) => { if (!controller.signal.aborted) setError(error.message); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [repository, valid, retry]);
  if (!valid) return <p className="page-note">No Hugging Face model card is associated with these local weights.</p>;
  return <section className="model-readme" aria-label="Publisher model card">
    <div className="heading-with-help"><a href={`https://huggingface.co/${repository}`} target="_blank" rel="noreferrer">{repository}</a><HelpTip label="Publisher documentation">This model card is supplied by the publisher. Its claims do not establish compatibility with the selected runtime or hardware.</HelpTip></div>
    {loading && <p role="status">Reading the model card…</p>}
    {error && <div role="alert"><p>{error}</p><Button variant="outline" onClick={() => setRetry(retry + 1)}>Retry README</Button></div>}
    <Markdown urlTransform={(url, key) => {
      const safe = defaultUrlTransform(url);
      if (!safe || /^(?:[a-z]+:|\/\/|#)/i.test(safe)) return safe;
      return new URL(safe, `https://huggingface.co/${repository}/${key === "src" ? "resolve" : "blob"}/main/`).href;
    }} remarkPlugins={[remarkGfm]} skipHtml components={{ a: ({ children, ...props }) => <a {...props} target="_blank" rel="noreferrer">{children}</a> }}>{content}</Markdown>
  </section>;
}
