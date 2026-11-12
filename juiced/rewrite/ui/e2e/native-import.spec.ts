import { api, expect, test } from "./support";

type Row = { native_id: string; proposed_id: string; importable: boolean; previous_policy: string | null; status: string; already_imported: string | null };
type Preview = { source: string; configuration_generation: number; models: Row[] };

test("settings from an old Juiced folder are previewed and imported through the API", async ({ request }) => {
  const service = api(request);
  const fixture = await service.source();
  const source = `${fixture.import_root}/old-juiced/config`;
  const preview = async () => service.post<Preview>("/v1/juiced/import/native", { action: "preview", source });
  const proposed = (await preview()).models.find((row) => row.native_id === "user.OldChat")!.proposed_id;
  await service.remove(proposed);
  try {
    const before = await preview();
    const row = before.models.find((entry) => entry.native_id === "user.OldChat")!;
    expect(row).toMatchObject({ importable: true, previous_policy: "on-demand", already_imported: null });
    const result = await service.post<{ models: { native_id: string; id: string; status: string }[] }>("/v1/juiced/import/native", {
      action: "apply", source: before.source, expected_generation: before.configuration_generation, aliases: true,
      models: [{ native_id: row.native_id, id: row.proposed_id, policy: "on-demand", include: true }],
    });
    expect(result.models).toContainEqual(expect.objectContaining({ native_id: "user.OldChat", id: proposed, status: "imported" }));
    expect((await service.model(proposed))?.policy).toBe("on-demand");
    expect((await preview()).models.find((entry) => entry.native_id === "user.OldChat")).toMatchObject({ status: "already_imported", already_imported: proposed });
  } finally {
    await service.remove(proposed);
  }
});
