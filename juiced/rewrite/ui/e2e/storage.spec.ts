import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { api, expect, navigate, noHorizontalOverflow, open, tab, test, toasts } from "./support";

test("Storage shows volumes and usage, and deletes a v0.9.2 leftover only after a preview", async ({ page, request }) => {
  const root = dirname((await api(request).source()).model_file);
  await mkdir(join(root, "prompt-state"), { recursive: true });
  await writeFile(join(root, "prompt-state", "slot-0.bin"), new Uint8Array(12 * 1024));
  await open(page);
  await navigate(page, "Engines & hardware");
  await tab(page, "Engines & hardware views", "Storage").click();
  const volumes = page.getByRole("region", { name: "Volumes" });
  await expect(volumes.getByRole("group").first()).toContainText(/free of/);
  const usage = page.getByRole("list", { name: "Usage by store" });
  await expect(usage.getByRole("listitem", { name: "Model weights" })).toBeVisible();
  await expect(usage.getByRole("listitem", { name: "Session cache" })).toContainText(/budget/);
  await expect(usage.getByRole("listitem", { name: "v0.9.2 leftovers" })).toContainText("reclaimable");
  const items = page.getByRole("list", { name: "Reclaimable items" });
  const leftover = items.getByRole("listitem", { name: "v0.9.2 prompt snapshots" });
  await expect(leftover).toContainText("12.0 KiB");
  await expect(items.getByRole("listitem", { name: "v0.9.2 engine builds and caches" })).toBeVisible();
  const review = page.getByRole("region", { name: "Reclaim space" }).getByRole("button", { name: /^Review/ });
  await expect(review).toBeDisabled();
  await noHorizontalOverflow(page, "Storage");

  await leftover.getByRole("checkbox").check();
  await expect(review).toHaveText("Review 12.0 KiB");
  await review.click();
  const dialog = page.getByRole("dialog", { name: "Delete these files?" });
  await expect(dialog).toContainText("Frees 12.0 KiB");
  await expect(dialog.getByRole("list", { name: "Items to delete" })).toContainText("v0.9.2 prompt snapshots");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(leftover).toBeVisible();

  await review.click();
  await dialog.getByRole("button", { name: "Delete 1 item" }).click();
  await expect(toasts(page)).toContainText("Freed 12.0 KiB.");
  await expect(items.getByRole("listitem", { name: "v0.9.2 prompt snapshots" })).toHaveCount(0);
  await expect(items.getByRole("listitem", { name: "v0.9.2 engine builds and caches" })).toBeVisible();
  await expect(page.getByRole("button", { name: /refresh/i })).toHaveCount(0);
});
