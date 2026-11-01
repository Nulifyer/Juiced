import { VoicePicker } from "./VoicePicker";
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
  Copy,
  Download,
  Upload,
  Columns2,
} from "lucide-react";
import "./Playground.css";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Button } from "./t3/button";
import { api, wav16k, type Model, type Message } from "../api";
type Props = {
  models: Model[];
  task: string;
  notify: (s: string) => void;
  openModels: () => void;
  initialModel?: string;
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
  task,
  notify,
  openModels,
  initialModel,
}: Props) {
  const [cacheMode, setCacheMode] = useState("memory");
  const [history, setHistory] = useState<Message[]>([]);
  const [model, setModel] = useState(initialModel || ""),
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
    [advanced, setAdvanced] = useState(false),
    [sourceAudio, setSourceAudio] = useState(""),
    [sourceName, setSourceName] = useState(""),
    [compareCurrent, setCompareCurrent] = useState("");
  const [resetKey, setResetKey] = useState(0);
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
    if (!eligible.some((m) => m.id === model)) setModel(eligible[0]?.id || "");
  }, [models, task]);
  useEffect(() => {
    abort.current?.abort();
    abort.current = null;
    setWorking(false);
    setHistory([]);
    setDraft(drafts.current[task] || "");
    setAttachments([]);
    setReceipt(undefined);
    setContent("");
    setReasoning("");
    if (recorder.current?.state === "recording") {
      recorder.current.onstop = null;
      recorder.current.stop();
      media.current?.getTracks().forEach((track) => track.stop());
      setRecording(false);
    }
  }, [task, model, resetKey]);
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
  useEffect(
    () => () => {
      if (sourceAudio) URL.revokeObjectURL(sourceAudio);
    },
    [sourceAudio],
  );
  function edit(s: string) {
    setDraft(s);
    drafts.current[task] = s;
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
      signal.throwIfAborted();
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
    const controller = new AbortController();
    abort.current = controller;
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
          controller.signal,
        );
        controller.signal.throwIfAborted();
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
          setCompareCurrent(id);
          const out = await completion(
            id,
            [message],
            controller.signal,
            () => {},
          );
          controller.signal.throwIfAborted();
          setComparison((p) => [...p, { ...out, id }]);
        }
        return;
      }
      const messages = [...history, message];
      setHistory(messages);
      edit("");
      setAttachments([]);
      const out = await completion(
        model,
        messages,
        controller.signal,
        (c, r) => {
          setContent(c);
          setReasoning(r);
        },
      );
      controller.signal.throwIfAborted();
      setReceipt(out.receipt);
      setHistory([
        ...messages,
        {
          role: "assistant",
          content: out.answer,
          ...(out.thinking ? { reasoning_content: out.thinking } : {}),
        },
      ]);
      setContent("");
      setReasoning("");
    } catch (e) {
      if (abort.current !== controller) return;
      if ((e as Error).name === "AbortError") notify("Generation stopped.");
      else notify((e as Error).message);
    } finally {
      if (abort.current === controller) {
        setWorking(false);
        abort.current = null;
        setCompareCurrent("");
      }
    }
  }
  async function speak(text: string, id?: string) {
    const target = id || speakers[0]?.id;
    if (!target) {
      notify("Load a speech model first.");
      return;
    }
    const controller = abort.current || new AbortController();
    abort.current = controller;
    const start = performance.now();
    const r = await fetch("/v1/audio/speech", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      signal: controller.signal,
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
    controller.signal.throwIfAborted();
    setAudio(URL.createObjectURL(blob));
    setReceipt({ model: target, total_ms: performance.now() - start });
  }
  async function transcribe(blob: Blob) {
    const target = task === "Transcribe" ? model : transcribers[0]?.id;
    if (!target) {
      notify("Load a recognizer before using audio input.");
      return;
    }
    const controller = new AbortController();
    abort.current = controller;
    setWorking(true);
    if (task === "Transcribe") {
      setSourceAudio(URL.createObjectURL(blob));
      setSourceName(blob instanceof File ? blob.name : "Microphone recording");
    }
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
          signal: controller.signal,
        });
      const j = await r.json();
      if (!r.ok)
        throw Error(j.error?.message || j.error || "Transcription failed");
      controller.signal.throwIfAborted();
      edit(j.text || "");
      setReceipt({ model: target, total_ms: performance.now() - start });
      notify("Transcript ready to review.");
    } catch (e) {
      if (abort.current === controller) notify((e as Error).message);
    } finally {
      if (abort.current === controller) {
        setWorking(false);
        abort.current = null;
      }
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
  function copyText() {
    void navigator.clipboard
      .writeText(draft)
      .then(() => notify("Transcript copied."))
      .catch(() =>
        notify("Could not copy. Select the text and copy it manually."),
      );
  }
  const downloadText = () => {
    const url = URL.createObjectURL(new Blob([draft], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "transcript.txt";
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  if (task !== "Chat")
    return (
      <div className={"service-workspace task-" + task.toLowerCase()}>
        <header className="page-heading">
          <div>
            <h1>
              {
                {
                  Transcribe: "Transcribe audio",
                  Speak: "Text to speech",
                  Images: "Generate an image",
                  Compare: "Compare models",
                }[task]
              }
            </h1>
            <p>
              {
                {
                  Transcribe: "Turn a recording into editable text.",
                  Speak:
                    "Write a script, choose a voice and listen to the result.",
                  Images: "Describe the scene and preview the generated image.",
                  Compare:
                    "Run the same prompt through your loaded chat models.",
                }[task]
              }
            </p>
          </div>
          <Button variant="outline" size="compact" onClick={openModels}>
            Manage models
          </Button>
        </header>
        {task !== "Compare" && (
          <div className="service-model-bar">
            <label>
              Model
              <select
                aria-label="Loaded model"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                disabled={working}
              >
                {!eligible.length && (
                  <option value="">No loaded model for this service</option>
                )}
                {eligible.map((m) => (
                  <option key={m.id}>{m.id}</option>
                ))}
              </select>
            </label>
            <span
              className={"runtime-indicator " + (selected ? "is-ready" : "")}
            >
              <i />
              {selected ? "Loaded" : "Load a model to begin"}
            </span>
          </div>
        )}
        {task === "Transcribe" && (
          <div className="transcription-studio">
            <section className="audio-source">
              <h2>Source audio</h2>
              <div
                className="audio-drop"
                onDragOver={(e) => e.preventDefault()}
                onDrop={(e) => {
                  e.preventDefault();
                  const f = e.dataTransfer.files[0];
                  if (f && !working && model) void transcribe(f);
                }}
              >
                <Upload size={25} />
                <h3>Drop an audio file</h3>
                <p>Or select a recording from your computer.</p>
                <Button
                  variant="outline"
                  disabled={working || !model}
                  onClick={() => audioInput.current?.click()}
                >
                  Choose audio
                </Button>
                <small>English · up to 2 minutes</small>
              </div>
              <div className="source-divider">
                <span>or</span>
              </div>
              <Button
                variant={recording ? "default" : "outline"}
                disabled={working || !model}
                onClick={() => void toggleMic()}
              >
                {recording ? <Square size={14} /> : <Mic size={15} />}
                {recording ? "Stop recording" : "Record microphone"}
              </Button>
              {sourceAudio && (
                <div className="source-recording">
                  <strong>{sourceName}</strong>
                  <audio controls src={sourceAudio} />
                </div>
              )}
              <p className="studio-footnote">
                Your recording is converted locally before transcription.
              </p>
            </section>
            <section className="transcript-output">
              <header>
                <h2>Transcript</h2>
                <div>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label="Copy transcript"
                    disabled={!draft}
                    onClick={copyText}
                  >
                    <Copy size={15} />
                  </Button>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label="Download transcript"
                    disabled={!draft}
                    onClick={downloadText}
                  >
                    <Download size={15} />
                  </Button>
                </div>
              </header>
              <textarea
                aria-label="Transcript"
                value={draft}
                onChange={(e) => edit(e.target.value)}
                placeholder={
                  working
                    ? "Transcribing your recording…"
                    : "Your transcript will appear here. You can edit it before copying or downloading."
                }
              />
              <footer>
                <span>
                  {working ? (
                    <>
                      <LoaderCircle className="spin" size={13} />
                      Transcribing…
                    </>
                  ) : draft ? (
                    `${draft.trim().split(/\s+/).length} words`
                  ) : (
                    "No recording transcribed yet"
                  )}
                </span>
                {receipt && (
                  <span>{(receipt.total_ms / 1000).toFixed(2)} s</span>
                )}
              </footer>
            </section>
          </div>
        )}
        {(task === "Speak" || task === "Images") && (
          <div className="generation-studio">
            <section className="generation-input">
              <h2>{task === "Speak" ? "Your script" : "Image prompt"}</h2>
              <textarea
                aria-label={task === "Speak" ? "Text to speak" : "Image prompt"}
                value={draft}
                onChange={(e) => edit(e.target.value)}
                placeholder={
                  task === "Speak"
                    ? "Write something you would like to hear…"
                    : "A scene, subject, lighting and any details that matter…"
                }
              />
              <div className="generation-controls">
                {task === "Speak" ? (
                  <VoicePicker
                    model={model}
                    value={voice}
                    onChange={setVoice}
                    disabled={working}
                  />
                ) : (
                  <span className="output-size">
                    Output size <strong>512 × 512</strong>
                  </span>
                )}
                <small>{draft.length.toLocaleString()} characters</small>
              </div>
              <Button
                disabled={working || !model || !draft.trim()}
                onClick={() => void send()}
              >
                {working ? (
                  <LoaderCircle className="spin" size={15} />
                ) : task === "Speak" ? (
                  <Volume2 size={15} />
                ) : (
                  <ImageIcon size={15} />
                )}
                {working
                  ? "Generating…"
                  : task === "Speak"
                    ? "Generate speech"
                    : "Generate image"}
              </Button>
              <p className="studio-footnote">
                {task === "Speak"
                  ? "Audio is generated by the selected speech model."
                  : "The selected model’s saved generation profile supplies the remaining settings."}
              </p>
            </section>
            <section
              className={
                "generation-output " +
                (task === "Images" ? "image-canvas" : "speech-preview")
              }
            >
              <header>
                <h2>{task === "Speak" ? "Listen" : "Preview"}</h2>
                {(task === "Speak" ? audio : image) && (
                  <a
                    className="output-download"
                    href={task === "Speak" ? audio : image}
                    download={
                      task === "Speak" ? "speech.wav" : "generated-image.png"
                    }
                  >
                    <Download size={14} />
                    Download
                  </a>
                )}
              </header>
              {task === "Speak" ? (
                audio ? (
                  <div className="speech-result">
                    <Volume2 size={38} />
                    <h3>Your audio is ready</h3>
                    <audio controls src={audio} />
                    <p>WAV audio · {voice}</p>
                  </div>
                ) : (
                  <div className="studio-placeholder">
                    <Volume2 size={34} />
                    <h3>Hear your model</h3>
                    <p>
                      Generate speech to listen here and download the audio.
                    </p>
                  </div>
                )
              ) : image ? (
                <img
                  className="generated-image"
                  src={image}
                  alt="Generated image"
                />
              ) : (
                <div className="studio-placeholder">
                  <ImageIcon size={34} />
                  <h3>Your image goes here</h3>
                  <p>Describe a scene, then generate a preview.</p>
                </div>
              )}
              {receipt && (
                <footer>
                  {working
                    ? "Generating…"
                    : `Generated in ${(receipt.total_ms / 1000).toFixed(2)} s`}
                </footer>
              )}
            </section>
          </div>
        )}
        {task === "Compare" && (
          <div className="comparison-workspace">
            <aside className="comparison-setup">
              <h2>Choose models</h2>
              <p>Select two or more to compare their responses.</p>
              {eligible.map((m) => (
                <label key={m.id}>
                  <input
                    type="checkbox"
                    disabled={working}
                    checked={compareModels.includes(m.id)}
                    onChange={(e) =>
                      setCompareModels((p) =>
                        e.target.checked
                          ? [...p, m.id]
                          : p.filter((x) => x !== m.id),
                      )
                    }
                  />
                  <span>
                    {m.id}
                    <small>{m.recipe}</small>
                  </span>
                </label>
              ))}
              {!eligible.length && <p>No chat models loaded.</p>}
              <div className="comparison-settings">
                <label>
                  Maximum output tokens
                  <input
                    type="number"
                    min={1}
                    max={16384}
                    value={maxTokens}
                    onChange={(e) =>
                      setMaxTokens(
                        Math.max(
                          1,
                          Math.min(16384, Number(e.target.value) || 1),
                        ),
                      )
                    }
                  />
                </label>
                <label>
                  Thinking
                  <select
                    value={thinkingMode}
                    onChange={(e) => setThinkingMode(e.target.value)}
                  >
                    <option value="profile">Model profile</option>
                    <option value="off">Off</option>
                    <option value="on">On</option>
                  </select>
                </label>
              </div>
              <p className="studio-footnote">
                Models run one at a time to avoid competing for the same
                hardware.
              </p>
            </aside>
            <section className="comparison-main">
              <div className="comparison-prompt">
                <label htmlFor="compare-prompt">Shared prompt</label>
                <textarea
                  id="compare-prompt"
                  value={draft}
                  onChange={(e) => edit(e.target.value)}
                  placeholder="Give each model exactly the same task…"
                />
                <div>
                  <span>{compareModels.length} models selected</span>
                  {working ? (
                    <Button
                      variant="outline"
                      onClick={() => abort.current?.abort()}
                    >
                      <Square size={13} />
                      Stop comparison
                    </Button>
                  ) : (
                    <Button
                      disabled={
                        !model || compareModels.length < 2 || !draft.trim()
                      }
                      onClick={() => void send()}
                    >
                      <Columns2 size={14} />
                      Run comparison
                    </Button>
                  )}
                </div>
              </div>
              {!comparison.length && !working ? (
                <div className="comparison-empty">
                  <Columns2 size={30} />
                  <h3>One prompt. Responses side by side.</h3>
                  <p>
                    Compare the output and observed response time without
                    switching conversations.
                  </p>
                </div>
              ) : (
                <div className="comparison-results">
                  {(working ? compareModels : comparison.map((r) => r.id)).map(
                    (id) => {
                      const r = comparison.find((r) => r.id === id);
                      return (
                        <article key={id}>
                          <header>
                            <strong>{id}</strong>
                            <span>
                              {r
                                ? `${(r.receipt.total_ms / 1000).toFixed(2)} s`
                                : compareCurrent === id
                                  ? "Generating…"
                                  : "Waiting"}
                            </span>
                          </header>
                          {r ? (
                            <>
                              <div className="markdown">
                                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                                  {r.answer}
                                </ReactMarkdown>
                              </div>
                              {r.thinking && (
                                <details className="thinking">
                                  <summary>Reasoning</summary>
                                  <pre>{r.thinking}</pre>
                                </details>
                              )}
                            </>
                          ) : (
                            <p className="page-note">
                              {compareCurrent === id
                                ? "The model is working on your prompt."
                                : "This model runs after the current response."}
                            </p>
                          )}
                        </article>
                      );
                    },
                  )}
                </div>
              )}
            </section>
          </div>
        )}
        <input
          ref={audioInput}
          type="file"
          hidden
          accept="audio/*"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void transcribe(f);
            e.target.value = "";
          }}
        />
      </div>
    );
  return (
    <div className="playground-layout">
      <header className="chat-header">
        <div>
          <h1>Test chat</h1>
          <p>Temporary chat test. Cleared when you leave or reload.</p>
        </div>
        <div className="model-picker">
          <select
            aria-label="Loaded model"
            value={model}
            onChange={(e) => setModel(e.target.value)}
            disabled={working}
          >
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
        <Button
          size="compact"
          variant="outline"
          onClick={() => {
            drafts.current = {};
            setResetKey((k) => k + 1);
            setAudio("");
          }}
        >
          Clear test
        </Button>
        <Button
          size="compact"
          variant="ghost"
          onClick={() => setAdvanced(!advanced)}
          aria-expanded={advanced}
        >
          Settings
        </Button>
      </header>
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
          <VoicePicker
            model={speakers[0]?.id || ""}
            value={voice}
            onChange={setVoice}
            disabled={working}
          />
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
              {m.role === "user" ? "You" : model || "Model"}
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
            <h2>Start a conversation</h2>
            <p>
              Send a test prompt, image or recording. This page keeps results
              only while it is open.
            </p>
          </div>
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
            placeholder="Ask a question or test a model…"
            onKeyDown={(e) => {
              if (
                e.key === "Enter" &&
                !e.shiftKey &&
                !e.nativeEvent.isComposing
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
            {working ? (
              <Button
                variant="outline"
                size="icon-sm"
                aria-label="Stop generation"
                onClick={() => abort.current?.abort()}
              >
                <Square size={14} />
              </Button>
            ) : (
              <Button
                variant="default"
                size="icon-sm"
                aria-label="Send message"
                disabled={!model || (!draft.trim() && !attachments.length)}
                onClick={() => void send()}
              >
                <ArrowUp size={17} />
              </Button>
            )}
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
