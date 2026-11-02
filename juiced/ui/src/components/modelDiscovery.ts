export type Repository = {
  repository_id: string;
  display_name?: string;
  downloads?: number;
  description?: string;
  tags?: string[];
};
// Strip only known terminal weight formats. Model versions and fine-tune names
// remain distinct, and different publishers are never merged.
export function quantization(name: string) {
  return name
    .replace(/[-.]GGUF$/i, "")
    .match(
      /(?:^|[-.])((?:IQ|Q)\d(?:_[A-Z0-9]+)*|MXFP\d(?:_[A-Z0-9]+)*|BF16|FP16|F16|FP32|F32)(?:\.gguf)?$/i,
    )?.[1]
    ?.toUpperCase();
}
export function modelQuant(model: {
  id: string;
  checkpoints?: Record<string, string>;
  checkpoint?: string;
}) {
  const source = model.checkpoints?.main || model.checkpoint || "";
  const selector = source.slice(source.lastIndexOf(":") + 1);
  return quantization(selector) || quantization(model.id) || "";
}
export function variantName(base: string, quant: string) {
  const current = quantization(base);
  return current === quant.toUpperCase()
    ? base
    : `${current ? base.slice(0, -current.length).replace(/[-.]$/, "") : base}-${quant}`;
}
export function existingVariant<
  T extends {
    recipe: string;
    checkpoints?: Record<string, string>;
    checkpoint?: string;
  },
>(
  models: T[],
  repo: string,
  variant: { name: string; files?: string[]; primary_file?: string },
  recipe: string,
  projector: string,
) {
  const matches = new Set([
    `${repo}:${variant.name}`,
    ...[...(variant.files || []), variant.primary_file]
      .filter(Boolean)
      .map((file) => `${repo}:${file}`),
  ]);
  return models.find(
    (model) =>
      model.recipe === recipe &&
      matches.has(model.checkpoints?.main || model.checkpoint || "") &&
      (model.checkpoints?.mmproj || "") ===
        (projector ? `${repo}:${projector}` : ""),
  );
}
export function groupRepositories(repositories: Repository[]) {
  const groups = new Map<
    string,
    { key: string; name: string; publisher: string; repositories: Repository[] }
  >();
  for (const repository of repositories) {
    const [publisher, ...parts] = repository.repository_id.split("/");
    const repoName = parts.join("/").replace(/[-.]GGUF$/i, "");
    const quant = quantization(repoName);
    const stem = (
      quant ? repoName.slice(0, -quant.length).replace(/[-.]$/, "") : repoName
    ).replace(/[-.]GGUF$/i, "");
    const base = repository.tags
      ?.find((t) => t.startsWith("base_model:quantized:"))
      ?.slice("base_model:quantized:".length);
    const name = base?.split("/").pop() || stem;
    const key = `${publisher}/${base || stem}`.toLowerCase();
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        name,
        publisher: publisher || "Unknown publisher",
        repositories: [],
      };
      groups.set(key, group);
    }
    if (
      !group.repositories.some(
        (r) => r.repository_id === repository.repository_id,
      )
    )
      group.repositories.push(repository);
  }
  return [...groups.values()];
}
