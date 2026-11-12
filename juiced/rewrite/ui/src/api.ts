import { parseSnapshot, type Snapshot, type Model } from "./domain";
import { multipartPayloadBytes } from "./speech";

export class ApiError extends Error {
  constructor(message: string, readonly status: number, readonly code: string, readonly body?: unknown) { super(message); }
}

export function chatTestTokenLimit(model: Pick<Model, "request_limits">): number {
  const limit = model.request_limits?.output_tokens ?? 1024;
  if (!Number.isSafeInteger(limit) || limit <= 0) throw new ApiError("The model's test limit is invalid. Refresh service state.", 0, "invalid_model_limits");
  return Math.min(1024, limit);
}

export class Client {
  constructor(readonly base: string, private readonly key: string) {}

  async request<T>(path: string, body?: unknown, signal?: AbortSignal, session?: string): Promise<T> {
    return this.exchange(path, body === undefined ? undefined : JSON.stringify(body), body === undefined ? undefined : "application/json", signal, session);
  }

  async testChat(model: Pick<Model, "id" | "request_limits">, prompt: string, signal?: AbortSignal): Promise<string | null> {
    const response = await this.request<{ choices?: unknown }>("chat/completions", { model: model.id, messages: [{ role: "user", content: prompt }], stream: false, max_tokens: chatTestTokenLimit(model) }, signal);
    const choices = response && typeof response === "object" ? response.choices : undefined;
    const content = Array.isArray(choices) ? choices[0]?.message?.content : undefined;
    if (content === null || typeof content === "string") return content;
    throw new ApiError("The worker returned an unsupported chat response. Check the selected service's compatibility.", 200, "invalid_chat_response");
  }

  async streamChat(model: string, messages: { role: string; content: string }[], maxTokens: number, onUpdate: (update: ChatStreamUpdate) => void, signal?: AbortSignal): Promise<ChatStreamUpdate> {
    const headers: Record<string, string> = { "Content-Type": "application/json", Accept: "text/event-stream" };
    if (this.key) headers.Authorization = `Bearer ${this.key}`;
    const response = await fetch(`${this.base}/v1/chat/completions`, { method: "POST", headers, signal, cache: "no-store", redirect: "error", body: JSON.stringify({ model, messages, stream: true, max_tokens: maxTokens, stream_options: { include_usage: true } }) });
    if (!response.ok || !response.body) {
      let message = `The request failed (${response.status}).`;
      try { const value = await response.json(); message = value?.error?.message ?? message; } catch { /* Error bodies are optional. */ }
      throw new ApiError(message, response.status, "chat_failed");
    }
    const state: ChatStreamUpdate = { content: "", reasoning: "", done: false, outputTokens: null, decodeRate: null };
    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
    let buffer = "", received = 0;
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        received += value.length;
        if (received > 4 * 1024 * 1024) throw new ApiError("The reply exceeds the 4 MiB test limit.", 200, "response_limit");
        buffer += value;
        let boundary;
        while ((boundary = buffer.indexOf("\n\n")) >= 0) {
          const event = buffer.slice(0, boundary); buffer = buffer.slice(boundary + 2);
          for (const line of event.split("\n")) {
            if (!line.startsWith("data:")) continue;
            const data = line.slice(5).trim();
            if (data === "[DONE]") { state.done = true; continue; }
            let chunk: any;
            try { chunk = JSON.parse(data); } catch { continue; }
            const delta = chunk?.choices?.[0]?.delta;
            if (typeof delta?.content === "string") state.content += delta.content;
            if (typeof delta?.reasoning_content === "string") state.reasoning += delta.reasoning_content;
            if (typeof chunk?.usage?.completion_tokens === "number") state.outputTokens = chunk.usage.completion_tokens;
            if (typeof chunk?.timings?.predicted_per_second === "number") state.decodeRate = chunk.timings.predicted_per_second;
          }
          onUpdate({ ...state });
        }
      }
    } finally { reader.releaseLock(); }
    state.done = true;
    return state;
  }

  async multipart<T>(path: string, body: FormData, signal?: AbortSignal): Promise<T> {
    multipartPayloadBytes(body);
    return this.exchange(path, body, undefined, signal);
  }

  private async exchange<T>(path: string, body: BodyInit | undefined, contentType?: string, signal?: AbortSignal, session?: string): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.key) headers.Authorization = `Bearer ${this.key}`;
    if (session) headers["X-Juiced-Session"] = session;
    if (contentType) headers["Content-Type"] = contentType;
    const response = await fetch(`${this.base}/v1/${path}`, { method: body === undefined ? "GET" : "POST", headers, body, signal, cache: "no-store", redirect: "error" });
    const maximum = 8 * 1024 * 1024;
    const parts: Uint8Array[] = []; let size = 0;
    if (response.body) {
      const reader = response.body.getReader();
      try {
        while (true) {
          const { done, value } = await reader.read(); if (done) break;
          size += value.byteLength;
          if (size > maximum) { await reader.cancel(); throw new ApiError("The service response exceeds the 8 MiB limit. Retry with a smaller request.", response.status, "response_limit"); }
          parts.push(value);
        }
      } finally { reader.releaseLock(); }
    }
    const bytes = new Uint8Array(size); let offset = 0;
    for (const part of parts) { bytes.set(part, offset); offset += part.byteLength; }
    let value;
    try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); }
    catch { throw new ApiError(`The service returned an unreadable response (${response.status}).`, response.status, "invalid_json"); }
    if (!response.ok) throw new ApiError(value?.error?.message ?? `The request failed (${response.status}).`, response.status, value?.error?.code ?? "request_failed", value);
    return value as T;
  }

  async snapshot(signal?: AbortSignal): Promise<Snapshot> {
    const [catalog, state, requests] = await Promise.all([
      this.request("juiced/catalog", undefined, signal), this.request("juiced/state", undefined, signal), this.request("juiced/requests", undefined, signal),
    ]);
    return parseSnapshot(catalog, state, requests);
  }
}

export type ChatStreamUpdate = { content: string; reasoning: string; done: boolean; outputTokens: number | null; decodeRate: number | null };
