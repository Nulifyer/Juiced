export type ImagePart = { type: "image_url"; image_url: { url: string } };
export type TextPart = { type: "text"; text: string };
export type MessageContent = string | (TextPart | ImagePart)[];
export type Attachment = { id: string; name: string; url: string; bytes: number };

export const MAX_IMAGES = 4;
export const MAX_EDGE = 1280;
/** The service accepts JSON request bodies up to 2 MiB; the rest of the request needs room too. */
export const REQUEST_LIMIT_BYTES = 2 * 1024 * 1024 - 16 * 1024;

// Adapted from T3 Code apps/web/src/components/chat/composerAttachmentFiles.ts.
// Copyright (c) 2026 T3 Tools Inc. MIT License; see licenses/LICENSE.t3code.
const IMAGE_TYPE_BY_EXTENSION: Readonly<Record<string, string>> = { gif: "image/gif", jpeg: "image/jpeg", jpg: "image/jpeg", png: "image/png", webp: "image/webp", bmp: "image/bmp" };

/** Drags from other apps and some pickers hand over files with an empty or generic type. */
export function imageType(file: Pick<File, "name" | "type">): string | null {
  const declared = file.type.toLowerCase();
  if (declared.startsWith("image/") && declared !== "image/svg+xml") return declared;
  if (declared && declared !== "application/octet-stream") return null;
  const dot = file.name.lastIndexOf(".");
  return dot > 0 ? IMAGE_TYPE_BY_EXTENSION[file.name.slice(dot + 1).toLowerCase()] ?? null : null;
}

export function fitWithin(width: number, height: number, edge = MAX_EDGE): { width: number; height: number } {
  const scale = Math.min(1, edge / Math.max(width, height, 1));
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

export function withImages(text: string, images: Pick<Attachment, "url">[]): MessageContent {
  if (images.length === 0) return text;
  return [...(text ? [{ type: "text" as const, text }] : []), ...images.map((image) => ({ type: "image_url" as const, image_url: { url: image.url } }))];
}

export function textOf(content: MessageContent | null | undefined): string {
  if (typeof content === "string") return content;
  return (content ?? []).map((part) => part.type === "text" ? part.text : "").join("");
}

export function imagesOf(content: MessageContent | null | undefined): string[] {
  return typeof content === "string" || !content ? [] : content.flatMap((part) => part.type === "image_url" ? [part.image_url.url] : []);
}

/** Drops images from a conversation, for storage that cannot hold them. */
export function withoutImages<T extends { content?: unknown }>(messages: T[]): T[] {
  return messages.map((message) => Array.isArray(message.content) ? { ...message, content: textOf(message.content as MessageContent) || "(image)" } : message);
}

export function requestBytes(body: unknown): number {
  return new TextEncoder().encode(JSON.stringify(body)).byteLength;
}

export const tooLarge = "Images in this chat exceed the 2 MiB request limit. Remove an image or start a new chat.";

/** Decodes a picked, pasted or dropped image and re-encodes it as a JPEG no larger than MAX_EDGE. */
export async function readImage(file: File): Promise<Attachment> {
  if (!imageType(file)) throw new Error(`${file.name || "That file"} is not an image.`);
  let bitmap: ImageBitmap;
  try { bitmap = await createImageBitmap(file); }
  catch { throw new Error(`${file.name || "The image"} could not be read.`); }
  const size = fitWithin(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = size.width; canvas.height = size.height;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("This browser cannot prepare images.");
  // JPEG has no alpha, so transparent areas would otherwise turn black.
  context.fillStyle = "#fff";
  context.fillRect(0, 0, size.width, size.height);
  context.drawImage(bitmap, 0, 0, size.width, size.height);
  bitmap.close();
  const url = canvas.toDataURL("image/jpeg", 0.85);
  return { id: `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`, name: file.name || "Pasted image", url, bytes: url.length };
}
