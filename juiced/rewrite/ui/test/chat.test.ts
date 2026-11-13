import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ChatMarkdown } from "../src/ChatMarkdown";
import { REQUEST_LIMIT_BYTES, fitWithin, imageType, imagesOf, requestBytes, textOf, withImages, withoutImages } from "../src/chatImages";
import { parseSnapshot } from "../src/domain";
import { splitMarkdownBlocks } from "../src/markdownBlocks";

const html = (text: string, streaming = false) => renderToStaticMarkup(createElement(ChatMarkdown, { text, streaming }));

describe("chat markdown", () => {
  test("renders GFM tables, fenced code with a copy control, lists and emphasis", () => {
    const out = html("| Model | Size |\n| --- | ---: |\n| a | 1 GiB |\n\n```bash\necho hi\n```\n\n- **one**\n- `two`");
    expect(out).toContain('<div class="md-table"><table>');
    expect(out).toContain("<th>Model</th>");
    expect(out).toContain('<td style="text-align:right">1 GiB</td>');
    expect(out).toContain('data-language="bash"');
    expect(out).toContain("<code>echo hi</code>");
    expect(out).toContain('aria-label="Copy code"');
    expect(out).toContain("<strong>one</strong>");
    expect(out).not.toContain("| a |");
  });

  test("keeps links safe and never loads remote images or raw HTML", () => {
    const out = html('[ok](https://example.com) [bad](javascript:alert(1)) ![pic](https://example.com/x.png) <img src=x onerror="alert(1)"> <script>alert(1)</script>');
    expect(out).toContain('<a href="https://example.com" target="_blank" rel="noreferrer noopener">ok</a>');
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("<img");
    expect(out).not.toContain("<script");
    expect(out).toContain('<a href="https://example.com/x.png"');
  });

  test("an unclosed fence while streaming still renders as code", () => {
    expect(html("Here:\n\n```js\nconst a = 1", true)).toContain("<code>const a = 1</code>");
  });

  test("splits finished blocks so only the last one changes while streaming", () => {
    expect(splitMarkdownBlocks("one\n\ntwo\n\n- a\n\n- b\n\nend")).toEqual(["one\n\n", "two\n\n- a\n\n- b\n\n", "end"]);
    expect(splitMarkdownBlocks("```\na\n\nb\n```\n\nafter")).toEqual(["```\na\n\nb\n```\n\n", "after"]);
    expect(splitMarkdownBlocks("see [x]\n\n[x]: https://example.com")).toBeNull();
  });
});

describe("chat images", () => {
  test("accepts raster images, infers a missing type from the name and refuses SVG", () => {
    expect(imageType({ name: "a.png", type: "image/png" })).toBe("image/png");
    expect(imageType({ name: "shot.JPG", type: "" })).toBe("image/jpeg");
    expect(imageType({ name: "shot.webp", type: "application/octet-stream" })).toBe("image/webp");
    expect(imageType({ name: "logo.svg", type: "image/svg+xml" })).toBeNull();
    expect(imageType({ name: "notes.txt", type: "text/plain" })).toBeNull();
  });

  test("fits images within the long edge without upscaling", () => {
    expect(fitWithin(4000, 2000)).toEqual({ width: 1280, height: 640 });
    expect(fitWithin(800, 600)).toEqual({ width: 800, height: 600 });
  });

  test("builds OpenAI content parts and reads them back", () => {
    expect(withImages("hi", [])).toBe("hi");
    const parts = withImages("what is this", [{ url: "data:image/jpeg;base64,AAAA" }]);
    expect(parts).toEqual([{ type: "text", text: "what is this" }, { type: "image_url", image_url: { url: "data:image/jpeg;base64,AAAA" } }]);
    expect(withImages("", [{ url: "data:x" }])).toEqual([{ type: "image_url", image_url: { url: "data:x" } }]);
    expect(textOf(parts)).toBe("what is this");
    expect(imagesOf(parts)).toEqual(["data:image/jpeg;base64,AAAA"]);
    expect(withoutImages([{ role: "user", content: parts }, { role: "user", content: withImages("", [{ url: "data:x" }]) }])).toEqual([{ role: "user", content: "what is this" }, { role: "user", content: "(image)" }]);
  });

  test("measures the request against the service's 2 MiB JSON limit", () => {
    expect(requestBytes({ text: "λ" })).toBe(13);
    expect(REQUEST_LIMIT_BYTES).toBeLessThan(2 * 1024 * 1024);
  });

  test("the model view's vision flag must be a boolean", () => {
    const state = { schema_version: 1, implementation: "rust-experiment", version: "1", resources: { reserved_bytes: 0 } };
    const model = { id: "m", policy: "hot", revision: 1, operations: ["chat/completions"], vision: true };
    expect(parseSnapshot({ schema_version: 1, data: [model] }, state, { data: [] }).models[0]!.vision).toBe(true);
    expect(() => parseSnapshot({ schema_version: 1, data: [{ ...model, vision: "yes" }] }, state, { data: [] })).toThrow("invalid model catalog");
  });
});
