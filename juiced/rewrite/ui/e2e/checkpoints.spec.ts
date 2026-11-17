import type { APIRequestContext, Page } from "@playwright/test";
import { api, expect, modelsView, open, test } from "./support";

const repo = "fixture-labs/Tiny-Classifier-ONNX";
const added = "Tiny-Classifier-ONNX";

async function checkpoint(request: APIRequestContext) {
  const search = await api(request).json<any>("/v1/juiced/library/search?q=tiny-classifier&engine=onnxruntime");
  return search.data.find((model: any) => model.repo === repo).quants[0];
}

async function cleanup(request: APIRequestContext) {
  const service = api(request);
  await service.remove(added);
  const quant = await checkpoint(request);
  if (quant.local?.set) await service.removeSet(quant.local.set);
  for (const file of quant.files) if (file.sha256) await service.removeArtifact(file.sha256);
}

async function select(page: Page) {
  await open(page);
  await modelsView(page, "Discover");
  await page.getByRole("combobox", { name: "Engine" }).selectOption("onnxruntime");
  await page.getByRole("searchbox", { name: "Search models" }).fill("tiny-classifier");
  const result = page.getByRole("list", { name: "Search results" }).getByRole("listitem").filter({ hasText: "Tiny-Classifier-ONNX" });
  await result.getByRole("button", { name: /^ONNX · ONNX Runtime/ }).click();
  const detail = page.getByRole("complementary", { name: "Model detail" });
  await expect(detail.getByRole("heading", { name: "Tiny-Classifier-ONNX" })).toBeVisible();
  return detail;
}

test("Discover downloads an ONNX checkpoint folder and adds it on ONNX Runtime when the download ends", async ({ page, request }) => {
  test.setTimeout(120000);
  await cleanup(request);
  try {
    const detail = await select(page);
    await expect(detail.getByText("Runs on", { exact: true })).toBeVisible();
    await expect(detail.locator("dd").filter({ hasText: /^ONNX Runtime$/ })).toBeVisible();
    const files = detail.getByRole("list", { name: "Checkpoint files" }).getByRole("listitem");
    await expect(files).toHaveCount(5);
    await expect(files.first()).toContainText("model.onnx");
    await expect(detail.getByText("README.md")).toHaveCount(0);

    await expect(detail.getByRole("radiogroup", { name: "Loading policy" }).getByRole("radio", { name: "On demand" })).toHaveAttribute("aria-checked", "true");
    const pulling = page.waitForRequest((outgoing) => outgoing.method() === "POST" && new URL(outgoing.url()).pathname === "/v1/juiced/library/pull");
    await detail.getByRole("button", { name: "Download" }).click();
    expect((await pulling).postDataJSON()).toMatchObject({ repo, file: "model.onnx", then: { policy: "on-demand", id: added } });
    await expect(detail.getByRole("status").filter({ hasText: `Added ${added}.` })).toBeVisible({ timeout: 60000 });
    await expect(detail.getByRole("option", { name: /ONNX/ })).toContainText("On demand");
    expect((await api(request).json<any>("/v1/juiced/engines")).installed.some((row: any) => row.engine === "onnxruntime"), "the service installs the engine it picked").toBe(true);
    const model = await api(request).model(added);
    expect(model?.policy).toBe("on-demand");
  } finally {
    await cleanup(request);
  }
});
