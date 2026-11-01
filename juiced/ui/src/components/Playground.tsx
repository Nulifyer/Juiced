import { useEffect, useRef, useState } from "react";
import {
  ArrowUp,
  Square,
  Plus,
  Mic,
  Volume2,
  LoaderCircle,
  X,
  Image as ImageIcon,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Button } from "./t3/button";
import { api, wav16k, type Model, type Message, type Thread } from "../api";
type Props = {
  models: Model[];
  thread: Thread | null;
  task: string;
  notify: (s: string) => void;
  onThread: (t: Thread, activate?: boolean) => void;
  openModels: () => void;
};
function textOf(m: Message) {
  return typeof m.content === "string"
    ? m.content
    : m.content
        .filter((c) => c.type === "text")
        .map((c) => c.text)
        .join("\n");
}
export function Playground({
  models,
  thread,
  task,
  notify,
  onThread,
  openModels,
}: Props) {
  const [cacheMode, setCacheMode] = useState("memory");
  const [model, setModel] = useState(""),
    [draft, setDraft] = useState(""),
    [working, setWorking] = useState(false),
    [content, setContent] = useState(""),
    [reasoning, setReasoning] = useState(""),
    [attachments, setAttachments] = useState<string[]>([]),
    [recording, setRecording] = useState(false),
    [voice, setVoice] = useState("af_heart"),
    [audio, setAudio] = useState(""),
    [image, setImage] = useState(""),
    [receipt, setReceipt] = useState<any>(),
    [maxTokens, setMaxTokens] = useState(2048),
    [thinkingMode, setThinkingMode] = useState("profile"),
    [compareModels, setCompareModels] = useState<string[]>([]),
    [comparison, setComparison] = useState<any[]>([]),
    [advanced, setAdvanced] = useState(false);
  const activeTurn = useRef<{ thread: string | null; task: string } | null>(
    null,
  );
  const abort = useRef<AbortController | null>(null),
    recorder = useRef<MediaRecorder | null>(null),
    media = useRef<MediaStream | null>(null),
    end = useRef<HTMLDivElement>(null),
    drafts = useRef<Record<string, string>>({}),
    attachmentInput = useRef<HTMLInputElement>(null),
    audioInput = useRef<HTMLInputElement>(null);
  const eligible = models.filter(
    (m) =>
      m.loaded &&
      m.policy !== "disabled" &&
      (task === "Compare" ? m.service === "Chat" : m.service === task),
  );
  const selected = eligible.find((m) => m.id === model),
    transcribers = models.filter(
      (m) => m.loaded && m.service === "Transcribe" && m.policy !== "disabled",
    ),
    speakers = models.filter(
      (m) => m.loaded && m.service === "Speak" && m.policy !== "disabled",
    );
  useEffect(() => {
    void api("juiced/cache")
      .then((j) => setCacheMode(j.settings.mode))
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (!eligible.some((m) => m.id === model))
      setModel(
        task === "Chat" && thread?.model
          ? eligible.find((m) => m.id === thread.model)?.id || ""
          : eligible[0]?.id || "",
      );
  }, [models, task, thread?.id]);
  useEffect(() => {
    if (task === "Chat" && thread?.model)
      setModel(eligible.find((m) => m.id === thread.model)?.id || "");
    if (
      activeTurn.current &&
      (activeTurn.current.thread !== (thread?.id || null) ||
        activeTurn.current.task !== task)
    )
      abort.current?.abort();
    setDraft(drafts.current[task === "Chat" ? thread?.id || task : task] || "");
    setAttachments([]);
    setReceipt(undefined);
    setContent("");
    setReasoning("");
  }, [thread?.id, task]);
  useEffect(
    () => () => {
      abort.current?.abort();
      media.current?.getTracks().forEach((t) => t.stop());
    },
    [],
  );
  useEffect(() => {
    end.current?.scrollIntoView({ block: "nearest" });
  }, [content, working]);
  useEffect(() => {
    return () => {
      if (audio) URL.revokeObjectURL(audio);
    };
  }, [audio]);
  function edit(s: string) {
    setDraft(s);
    drafts.current[task === "Chat" ? thread?.id || task : task] = s;
  }
  async function save(t: Thread, activate = true) {
    const result = await api<Thread>("juiced/thread", t);
    onThread(result, activate);
    return result;
  }
  async function attach(files: FileList | null) {
    if (!selected?.labels.includes("vision")) {
      notify("Choose a loaded vision model before attaching an image.");
      return;
    }
    for (const f of Array.from(files || [])) {
      if (!f.type.startsWith("image/") || f.size > 6 * 1024 * 1024) {
        notify("Use images up to 6 MiB each.");
        continue;
      }
      if (attachments.length >= 3) {
        notify("Use up to three images per message.");
        break;
      }
      const url = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result));
        r.onerror = reject;
        r.readAsDataURL(f);
      });
      setAttachments((prev) => (prev.length < 3 ? [...prev, url] : prev));
    }
  }
  async function completion(
    id: string,
    messages: Message[],
    signal: AbortSignal,
    onDelta: (c: string, r: string) => void,
    threadId?: string,
  ) {
    const start = performance.now();
    let first: number | undefined,
      answer = "",
      thinking = "",
      usage: any;
    const r = await fetch("/v1/chat/completions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(threadId ? { "X-Juiced-Thread-Id": threadId } : {}),
      },
      body: JSON.stringify({
        model: id,
        messages,
        max_tokens: maxTokens,
        stream: true,
        stream_options: { include_usage: true },
        cache_prompt: cacheMode !== "off",
        ...(thinkingMode === "profile"
          ? {}
          : {
              chat_template_kwargs: { enable_thinking: thinkingMode === "on" },
            }),
      }),
      signal,
    });
    if (!r.ok) {
      let j;
      try {
        j = await r.json();
      } catch {}
      throw Error(
        j?.error?.message || j?.error || "Inference failed (" + r.status + ").",
      );
    }
    const reader = r.body!.getReader(),
      decoder = new TextDecoder();
    let pending = "";
    const event = (line: string) => {
      if (!line.startsWith("data:")) return;
      const raw = line.slice(5).trim();
      if (raw === "[DONE]" || !raw) return;
      const j = JSON.parse(raw);
      if (j.error) throw Error(j.error.message || String(j.error));
      const delta = j.choices?.[0]?.delta || {};
      if (delta.content || delta.reasoning_content) {
        first ??= performance.now() - start;
        answer += delta.content || "";
        thinking += delta.reasoning_content || "";
        onDelta(answer, thinking);
      }
      if (j.usage) usage = j.usage;
    };
    while (true) {
      const { done, value } = await reader.read();
      pending += decoder.decode(value || new Uint8Array(), { stream: !done });
      let cut;
      while ((cut = pending.indexOf("\n")) >= 0) {
        event(pending.slice(0, cut));
        pending = pending.slice(cut + 1);
      }
      if (done) break;
    }
    if (pending.trim()) event(pending);
    return {
      answer,
      thinking,
      receipt: {
        model: id,
        total_ms: performance.now() - start,
        first_ms: first,
        usage,
        cache: r.headers.get("X-Juiced-Cache"),
        restored_tokens: r.headers.get("X-Juiced-Restored-Tokens"),
      },
    };
  }
  async function send() {
    if (!draft.trim() && !attachments.length) return;
    if (attachments.length && !selected?.labels.includes("vision")) {
      notify(
        "Remove the images or choose a loaded vision model before sending.",
      );
      return;
    }
    if (!model) {
      notify("Load an eligible model first.");
      return;
    }
    setWorking(true);
    setContent("");
    setReasoning("");
    setReceipt(undefined);
    abort.current = new AbortController();
    activeTurn.current = { thread: thread?.id || null, task };
    try {
      if (task === "Speak") {
        await speak(draft, model);
        return;
      }
      if (task === "Images") {
        const start = performance.now();
        const r = await api(
          "images/generations",
          {
            model,
            prompt: draft,
            n: 1,
            size: "512x512",
            response_format: "b64_json",
          },
          abort.current.signal,
        );
        const data = r.data?.[0];
        setImage(
          data?.b64_json
            ? "data:image/png;base64," + data.b64_json
            : data?.url || "",
        );
        setReceipt({ model, total_ms: performance.now() - start });
        return;
      }
      const message: Message = {
        role: "user",
        content: attachments.length
          ? [
              { type: "text", text: draft },
              ...attachments.map((url) => ({
                type: "image_url",
                image_url: { url },
              })),
            ]
          : draft,
      };
      if (task === "Compare") {
        setComparison([]);
        for (const id of compareModels) {
          const out = await completion(
            id,
            [message],
            abort.current.signal,
            () => {},
          );
          setComparison((p) => [...p, { ...out, id }]);
        }
        return;
      }
      let t =
        thread ||
        (await api<Thread>("juiced/thread", {
          title: draft.slice(0, 64) || "Image test",
          shelf: "working",
          model,
          messages: [],
        }));
      activeTurn.current = { thread: t.id, task };
      t = await save({ ...t, model, messages: [...t.messages, message] });
      edit("");
      setAttachments([]);
      const out = await completion(
        model,
        t.messages,
        abort.current.signal,
        (c, r) => {
          setContent(c);
          setReasoning(r);
        },
        t.id,
      );
      setReceipt(out.receipt);
      await save(
        {
          ...t,
          messages: [
            ...t.messages,
            {
              role: "assistant",
              content: out.answer,
              ...(out.thinking ? { reasoning_content: out.thinking } : {}),
            },
          ],
        },
        false,
      );
      setContent("");
      setReasoning("");
    } catch (e) {
      if ((e as Error).name === "AbortError")
        notify("Generation stopped. The submitted user message is retained.");
      else notify((e as Error).message);
    } finally {
      setWorking(false);
      abort.current = null;
      activeTurn.current = null;
    }
  }
  async function speak(text: string, id?: string) {
    const target = id || speakers[0]?.id;
    if (!target) {
      notify("Load a speech model first.");
      return;
    }
    const r = await fetch("/v1/audio/speech", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      signal: abort.current?.signal,
      body: JSON.stringify({
        model: target,
        input: text,
        voice,
        response_format: "wav",
      }),
    });
    if (!r.ok) throw Error("Speech synthesis failed (" + r.status + ").");
    const blob = await r.blob();
    if (!blob.size) throw Error("The speech backend returned no audio.");
    setAudio(URL.createObjectURL(blob));
  }
  async function transcribe(blob: Blob) {
    const target = task === "Transcribe" ? model : transcribers[0]?.id;
    if (!target) {
      notify("Load a recognizer before using audio input.");
      return;
    }
    setWorking(true);
    try {
      const wav = await wav16k(blob),
        form = new FormData();
      form.append("file", wav, "recording.wav");
      form.append("model", target);
      form.append("language", "en");
      form.append("response_format", "json");
      const start = performance.now(),
        r = await fetch("/v1/audio/transcriptions", {
          method: "POST",
          body: form,
        });
      const j = await r.json();
      if (!r.ok)
        throw Error(j.error?.message || j.error || "Transcription failed");
      edit(j.text || "");
      setReceipt({ model: target, total_ms: performance.now() - start });
      notify("Transcript ready to review.");
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setWorking(false);
    }
  }
  async function toggleMic() {
    if (recording) {
      recorder.current?.stop();
      setRecording(false);
      return;
    }
    if (!(task === "Transcribe" ? model : transcribers[0]?.id)) {
      notify("Load a recognizer first.");
      return;
    }
    try {
      media.current = await navigator.mediaDevices.getUserMedia({
        audio: true,
      });
      const chunks: BlobPart[] = [];
      recorder.current = new MediaRecorder(media.current);
      recorder.current.ondataavailable = (e) => {
        if (e.data.size) chunks.push(e.data);
      };
      recorder.current.onstop = () => {
        media.current?.getTracks().forEach((t) => t.stop());
        void transcribe(
          new Blob(chunks, {
            type: recorder.current?.mimeType || "audio/webm",
          }),
        );
      };
      recorder.current.start();
      setRecording(true);
    } catch (e) {
      notify((e as Error).message);
    }
  }
  const history = task === "Chat" ? thread?.messages || [] : [];
  return (
    <div className="playground-layout">
      <header className="chat-header">
        <div>
          <h1>
            {task === "Chat"
              ? thread?.title || "New chat"
              : task + " playground"}
          </h1>
          <p>
            {task === "Chat"
              ? "Direct model interaction · no agent tools"
              : task === "Compare"
                ? "Identical prompt, sequential runs to avoid interference"
                : "Use an eligible loaded deployment"}
          </p>
        </div>
        <Button
          size="compact"
          variant="ghost"
          onClick={() => setAdvanced(!advanced)}
          aria-expanded={advanced}
        >
          Settings
        </Button>
      </header>
      <div className="model-picker">
        <select
          aria-label="Loaded model"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          disabled={working}
        >
          {!model &&
            thread?.model &&
            task === "Chat" &&
            eligible.length > 0 && (
              <option value="">{thread.model} · unloaded</option>
            )}
          {eligible.length === 0 ? (
            <option value="">No eligible loaded models</option>
          ) : (
            eligible.map((m) => <option key={m.id}>{m.id}</option>)
          )}
        </select>
        <Button variant="outline" size="compact" onClick={openModels}>
          Load another…
        </Button>
      </div>
      {advanced && (
        <div className="inline-settings">
          <label>
            Maximum output tokens
            <input
              type="number"
              value={maxTokens}
              min={1}
              max={16384}
              onChange={(e) =>
                setMaxTokens(
                  Math.max(1, Math.min(16384, Number(e.target.value) || 1)),
                )
              }
            />
          </label>
          {(task === "Chat" || task === "Compare") && (
            <label>
              Thinking
              <select
                value={thinkingMode}
                onChange={(e) => setThinkingMode(e.target.value)}
              >
                <option value="profile">Saved model profile</option>
                <option value="off">Off · faster replies</option>
                <option value="on">
                  On · uses the model's reasoning budget
                </option>
              </select>
            </label>
          )}
          <label>
            Speech voice
            <input value={voice} onChange={(e) => setVoice(e.target.value)} />
          </label>
          <p className="page-note">
            Sampling and context use the selected deployment’s saved profile.
            Incompatible requests return an error.
          </p>
        </div>
      )}
      <div className="chat-messages" aria-live="polite">
        {history.map((m, i) => (
          <article className={"chat-message " + m.role} key={i}>
            <div className="message-meta">
              {m.role === "user" ? "You" : thread?.model || "Model"}
            </div>
            {m.reasoning_content && (
              <details className="thinking">
                <summary>Reasoning</summary>
                <pre>{m.reasoning_content}</pre>
              </details>
            )}
            <div className="markdown">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                {textOf(m)}
              </ReactMarkdown>
            </div>
            {Array.isArray(m.content) &&
              m.content
                .filter((c) => c.type === "image_url")
                .map((c, n) => (
                  <img
                    className="message-image"
                    key={n}
                    src={c.image_url?.url}
                    alt="Attached image"
                  />
                ))}
            {m.role === "assistant" && speakers.length > 0 && (
              <Button
                size="icon-xs"
                variant="ghost"
                aria-label="Read answer aloud"
                onClick={() =>
                  void speak(textOf(m)).catch((e) => notify(e.message))
                }
              >
                <Volume2 size={14} />
              </Button>
            )}
          </article>
        ))}
        {working && (
          <article className="chat-message assistant">
            <div className="message-meta">
              <LoaderCircle className="spin" size={13} /> {model}
            </div>
            {reasoning && (
              <details className="thinking">
                <summary>Reasoning</summary>
                <pre>{reasoning}</pre>
              </details>
            )}
            <div className="markdown">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                {content || "Waiting for the model…"}
              </ReactMarkdown>
            </div>
          </article>
        )}
        {!history.length && !working && task === "Chat" && (
          <div className="chat-empty">
            <MessageSquareIcon />
            <h2>Test a model directly.</h2>
            <p>
              Select a loaded model, then send text, attach a supported image or
              dictate a message.
            </p>
          </div>
        )}
        {task === "Transcribe" && (
          <div className="media-panel">
            <Button
              variant="outline"
              onClick={() => audioInput.current?.click()}
            >
              <UploadIcon />
              Choose audio
            </Button>
            <p>
              Audio is converted in your browser to 16 kHz WAV. English, up to
              120 seconds.
            </p>
            <p>The editable transcript appears below.</p>
          </div>
        )}
        {task === "Speak" && audio && <audio controls src={audio} />}
        {task === "Images" && image && (
          <img className="generated-image" src={image} alt="Generated image" />
        )}
        {task === "Compare" && (
          <>
            <div className="compare-choices">
              {eligible.map((m) => (
                <label key={m.id}>
                  <input
                    type="checkbox"
                    checked={compareModels.includes(m.id)}
                    onChange={(e) =>
                      setCompareModels((p) =>
                        e.target.checked
                          ? [...p, m.id]
                          : p.filter((x) => x !== m.id),
                      )
                    }
                  />
                  {m.id}
                </label>
              ))}
            </div>
            {comparison.map((r) => (
              <article className="compare-answer" key={r.id}>
                <h2>{r.id}</h2>
                <ReactMarkdown>{r.answer}</ReactMarkdown>
                <p className="page-note">
                  {(r.receipt.total_ms / 1000).toFixed(2)} s total
                </p>
              </article>
            ))}
          </>
        )}
        {task === "Chat" && audio && <audio controls src={audio} />}
        <div ref={end} />
      </div>
      <div className="composer-area">
        {attachments.length > 0 && (
          <div className="attachment-strip">
            {attachments.map((a, i) => (
              <div className="attachment-chip" key={i}>
                <ImageIcon size={13} />
                <span>Image {i + 1}</span>
                <button
                  aria-label={"Remove image " + (i + 1)}
                  onClick={() =>
                    setAttachments((p) => p.filter((_, n) => n !== i))
                  }
                >
                  <X size={12} />
                </button>
              </div>
            ))}
          </div>
        )}
        <div
          className="composer"
          onPaste={(e) => {
            if (e.clipboardData.files.length) {
              e.preventDefault();
              void attach(e.clipboardData.files);
            }
          }}
        >
          <textarea
            aria-label="Message"
            value={draft}
            onChange={(e) => edit(e.target.value)}
            placeholder={
              task === "Speak"
                ? "Text to speak…"
                : task === "Transcribe"
                  ? "Editable transcript…"
                  : "Ask a question or test a model…"
            }
            onKeyDown={(e) => {
              if (
                e.key === "Enter" &&
                !e.shiftKey &&
                !e.nativeEvent.isComposing &&
                task !== "Transcribe"
              ) {
                e.preventDefault();
                if (!working) void send();
              }
            }}
          />
          <div className="composer-toolbar">
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label="Attach image"
              disabled={working || !selected?.labels.includes("vision")}
              onClick={() => attachmentInput.current?.click()}
            >
              <Plus size={16} />
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label={recording ? "Stop recording" : "Record microphone"}
              aria-pressed={recording}
              disabled={working}
              onClick={() => void toggleMic()}
            >
              {recording ? <Square size={14} /> : <Mic size={15} />}
            </Button>
            <span className="composer-hint">
              {recording
                ? "Recording…"
                : task === "Chat"
                  ? "Enter to send · Shift+Enter for newline"
                  : "Review before sending"}
            </span>
            {task !== "Transcribe" &&
              (working ? (
                <Button
                  variant="outline"
                  size="icon-sm"
                  aria-label={
                    task === "Images" || task === "Speak"
                      ? "Generating media"
                      : "Stop generation"
                  }
                  disabled={task === "Images" || task === "Speak"}
                  title={
                    task === "Images" || task === "Speak"
                      ? "This media request completes before another can start"
                      : "Stop generation"
                  }
                  onClick={() => abort.current?.abort()}
                >
                  {task === "Images" || task === "Speak" ? (
                    <LoaderCircle className="spin" size={14} />
                  ) : (
                    <Square size={14} />
                  )}
                </Button>
              ) : (
                <Button
                  variant="default"
                  size="icon-sm"
                  aria-label="Send message"
                  disabled={
                    !model ||
                    (!draft.trim() && !attachments.length) ||
                    (task === "Compare" && !compareModels.length)
                  }
                  onClick={() => void send()}
                >
                  <ArrowUp size={17} />
                </Button>
              ))}
          </div>
        </div>
        {receipt && (
          <details className="receipt">
            <summary>
              Request receipt · {(receipt.total_ms / 1000).toFixed(2)} s total
            </summary>
            <div>Model: {receipt.model}</div>
            {receipt.first_ms !== undefined && (
              <div>First output: {(receipt.first_ms / 1000).toFixed(2)} s</div>
            )}
            {receipt.cache && (
              <div>
                Prompt state: {receipt.cache}
                {receipt.restored_tokens
                  ? ` · ${Number(receipt.restored_tokens).toLocaleString()} tokens restored`
                  : ""}
                . Actual reuse is reported separately by the engine.
              </div>
            )}
            {receipt.usage && (
              <pre>{JSON.stringify(receipt.usage, null, 2)}</pre>
            )}
            <p>
              Client-observed timing. Includes queue, network and backend work;
              it does not isolate prefill speed.
            </p>
          </details>
        )}
        <input
          ref={attachmentInput}
          type="file"
          hidden
          multiple
          accept="image/*"
          onChange={(e) => void attach(e.target.files)}
        />
        <input
          ref={audioInput}
          type="file"
          hidden
          accept="audio/*"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void transcribe(f);
          }}
        />
      </div>
    </div>
  );
}
function MessageSquareIcon() {
  return (
    <svg
      width="30"
      height="30"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
    >
      <path d="M3 3h18v13H9l-6 5z" />
    </svg>
  );
}
function UploadIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
    >
      <path d="M12 16V3m-5 5 5-5 5 5M3 16v5h18v-5" />
    </svg>
  );
}
