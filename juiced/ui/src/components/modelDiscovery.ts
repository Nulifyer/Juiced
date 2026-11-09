export type Repository = {
  repository_id: string;
  display_name?: string;
  downloads?: number;
  description?: string;
  tags?: string[];
  task?: string;
};

export function matchesModelSearch(query: string, values: string[]) {
  const normalize = (value: string) => value.normalize("NFKC").toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
  const text = normalize(values.join(" "));
  const compact = text.replaceAll(" ", "");
  return normalize(query).split(/\s+/).filter(Boolean).every((word) => text.includes(word) || compact.includes(word));
}

export function catalogEntries<T extends { id: string; recipe: string; checkpoint?: string; checkpoints?: Record<string, string>; available?: boolean; downloaded?: boolean; source?: string; registry_source?: string }>(models: T[]) {
  const identities = new Set(models.filter((m) => !m.id.startsWith("builtin.")).map((m) => JSON.stringify([m.id, m.recipe, m.checkpoints?.main || m.checkpoint, modelRegistrySource(m)])));
  return models.filter((m) => (m.available !== false || m.downloaded) && (!m.id.startsWith("builtin.") || !identities.has(JSON.stringify([m.id.slice(8), m.recipe, m.checkpoints?.main || m.checkpoint, modelRegistrySource(m)]))));
}
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
  quant = quantization(quant) || quant;
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
    source?: string;
    registry_source?: string;
  },
>(
  models: T[],
  repo: string,
  variant: { name: string; files?: string[]; primary_file?: string },
  recipe: string,
  projector: string,
  source: PublishedSource = "huggingface",
) {
  const matches = new Set([
    `${repo}:${variant.name}`,
    ...[...(variant.files || []), variant.primary_file]
      .filter(Boolean)
      .map((file) => `${repo}:${file}`),
  ]);
  return models.find(
    (model) =>
      modelRegistrySource(model) === source && model.recipe === recipe &&
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

export type PublishedSource = "huggingface" | "modelscope";
export function modelRegistrySource(model: { registry_source?: string; source?: string }): PublishedSource | undefined {
  const source = model.registry_source || model.source || "huggingface";
  return source === "huggingface" || source === "modelscope" ? source : undefined;
}
export function repositoryUrl(repository: string, source: PublishedSource = "huggingface") {
  return (source === "modelscope" ? "https://modelscope.cn/models/" : "https://huggingface.co/") + repository;
}
