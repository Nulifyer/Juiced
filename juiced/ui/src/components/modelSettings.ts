export type Options = Record<string, any>;
export type OptionResponse = {
  saved: Options;
  effective: Options;
  defaults: Options;
  recipe: string;
};
export type ArgField = {
  key: string;
  aliases: string[];
  label: string;
  help: string;
  min?: number;
  step?: number;
  choices?: string[];
};
export const runtimeFields: ArgField[] = [
  {
    key: "parallel",
    aliases: ["--parallel", "-np"],
    label: "Concurrent chats",
    help: "Context is divided between these slots.",
    min: 1,
  },
  {
    key: "gpu",
    aliases: ["-ngl", "--gpu-layers", "--n-gpu-layers"],
    label: "GPU layers",
    help: "999 offloads all available layers. 0 uses CPU.",
    min: 0,
  },
  {
    key: "threads",
    aliases: ["-t", "--threads"],
    label: "CPU threads",
    help: "Threads used during generation. -1 lets the backend choose.",
    min: -1,
  },
  {
    key: "batchThreads",
    aliases: ["-tb", "--threads-batch"],
    label: "Prompt threads",
    help: "CPU threads used to process a prompt.",
    min: -1,
  },
  {
    key: "batch",
    aliases: ["-b", "--batch-size"],
    label: "Batch size",
    help: "Maximum prompt tokens processed per batch.",
    min: 1,
  },
  {
    key: "ubatch",
    aliases: ["-ub", "--ubatch-size"],
    label: "Physical batch size",
    help: "Lower this to reduce peak memory use.",
    min: 1,
  },
  {
    key: "flash",
    aliases: ["-fa", "--flash-attn"],
    label: "Flash attention",
    help: "Support depends on the backend and model.",
    choices: ["auto", "on", "off"],
  },
  {
    key: "cacheK",
    aliases: ["-ctk", "--cache-type-k"],
    label: "Key cache precision",
    help: "Quantized caches use less memory and may affect quality.",
    choices: ["f16", "q8_0", "q4_0"],
  },
  {
    key: "cacheV",
    aliases: ["-ctv", "--cache-type-v"],
    label: "Value cache precision",
    help: "Quantized value caches may require flash attention.",
    choices: ["f16", "q8_0", "q4_0"],
  },
];
export const samplingFields: ArgField[] = [
  {
    key: "temperature",
    aliases: ["--temp", "--temperature"],
    label: "Temperature",
    help: "Higher values produce more varied responses.",
    min: 0,
    step: 0.05,
  },
  {
    key: "topP",
    aliases: ["--top-p"],
    label: "Top P",
    help: "Probability mass retained during sampling.",
    min: 0,
    step: 0.05,
  },
  {
    key: "topK",
    aliases: ["--top-k"],
    label: "Top K",
    help: "Candidate token count. 0 disables the limit.",
    min: 0,
  },
  {
    key: "minP",
    aliases: ["--min-p"],
    label: "Min P",
    help: "Minimum probability relative to the most likely token.",
    min: 0,
    step: 0.01,
  },
  {
    key: "repeat",
    aliases: ["--repeat-penalty"],
    label: "Repetition penalty",
    help: "1 leaves repetition unchanged.",
    min: 0,
    step: 0.05,
  },
  {
    key: "reasoning",
    aliases: ["--reasoning-budget"],
    label: "Reasoning token budget",
    help: "Backend default when blank. -1 allows unlimited reasoning.",
    min: -1,
  },
];
export const argFields = [...runtimeFields, ...samplingFields];

// Backend argument strings use quoting, but are never evaluated by a shell.
export function splitArgs(source: string): string[] {
  const tokens: string[] = [];
  let token = "",
    quote = "",
    escaped = false,
    started = false;
  for (let index = 0; index < source.length; index++) {
    const c = source[index];
    if (escaped) {
      token += c;
      escaped = false;
      started = true;
    } else if (c === "\\" && quote !== "'") {
      if (
        quote === '"' &&
        index + 1 < source.length &&
        !['"', "\\", "$", "`", "\n"].includes(source[index + 1])
      )
        token += c;
      else escaped = true;
      started = true;
    } else if (quote) {
      if (c === quote) quote = "";
      else token += c;
    } else if (c === "'" || c === '"') {
      quote = c;
      started = true;
    } else if (/\s/.test(c)) {
      if (started) {
        tokens.push(token);
        token = "";
        started = false;
      }
    } else {
      token += c;
      started = true;
    }
  }
  if (escaped || quote)
    throw Error("Finish the quoted or escaped backend argument before saving.");
  if (started) tokens.push(token);
  return tokens;
}
export function joinArgs(tokens: string[]) {
  return tokens
    .map((t) =>
      /^[A-Za-z0-9_./:=,+@%-]+$/.test(t)
        ? t
        : "'" + t.replace(/'/g, "'\\''") + "'",
    )
    .join(" ");
}
function keyOf(token: string) {
  return token.split("=", 1)[0];
}
export function readArg(source: string, field: ArgField) {
  const tokens = splitArgs(source);
  let result = "";
  for (let i = 0; i < tokens.length; i++)
    if (field.aliases.includes(keyOf(tokens[i]))) {
      const t = tokens[i];
      result = t.includes("=")
        ? t.slice(t.indexOf("=") + 1)
        : tokens[i + 1] || "";
    }
  return result;
}
export function stripArgs(source: string, fields = argFields) {
  const tokens = splitArgs(source),
    result: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    if (fields.some((f) => f.aliases.includes(keyOf(tokens[i])))) {
      if (
        !tokens[i].includes("=") &&
        i + 1 < tokens.length &&
        (!tokens[i + 1].startsWith("-") || /^-\d/.test(tokens[i + 1]))
      )
        i++;
    } else result.push(tokens[i]);
  }
  return joinArgs(result);
}
export function setArg(source: string, field: ArgField, value: string) {
  const tokens = splitArgs(stripArgs(source, [field]));
  if (value !== "") tokens.push(field.aliases[0], value);
  return joinArgs(tokens);
}
export function withExtraArgs(source: string, extra: string) {
  const tokens = splitArgs(extra);
  if (tokens.some((t) => argFields.some((f) => f.aliases.includes(keyOf(t)))))
    throw Error(
      "Use the controls above for context, performance and sampling arguments.",
    );
  const controlled = argFields.reduce((s, f) => {
    const value = readArg(source, f);
    return value === "" ? s : setArg(s, f, value);
  }, "");
  return joinArgs([...splitArgs(controlled), ...tokens]);
}
export function editableOptions(saved: Options) {
  return Object.fromEntries(
    Object.entries(saved).filter(
      ([key]) => key !== "pinned" && key !== "model_name",
    ),
  );
}
export function optionPatch(before: Options, after: Options) {
  const result: Options = {};
  for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) {
    if (key === "pinned" || key === "model_name") continue;
    if (JSON.stringify(before[key]) !== JSON.stringify(after[key]))
      result[key] = after[key] ?? null;
  }
  return result;
}
export function validateSettings(options: Options) {
  if (
    options.ctx_size !== undefined &&
    options.ctx_size !== null &&
    (!Number.isInteger(options.ctx_size) ||
      options.ctx_size === 0 ||
      options.ctx_size < -1)
  )
    throw Error(
      "Context must be a positive whole number, or -1 for automatic.",
    );
  if (typeof options.llamacpp_args !== "string") return;
  for (const field of argFields) {
    const value = readArg(options.llamacpp_args, field);
    if (!value || field.choices) continue;
    const n = Number(value);
    if (
      !Number.isFinite(n) ||
      (!field.step && !Number.isInteger(n)) ||
      (field.min !== undefined && n < field.min)
    )
      throw Error(
        `${field.label} must be a valid ${field.step ? "number" : "whole number"}${field.min !== undefined ? ` of at least ${field.min}` : ""}.`,
      );
    if (["topP", "minP"].includes(field.key) && n > 1)
      throw Error(`${field.label} must be between 0 and 1.`);
  }
  const batch = Number(
    readArg(
      options.llamacpp_args,
      runtimeFields.find((f) => f.key === "batch")!,
    ),
  );
  const ubatch = Number(
    readArg(
      options.llamacpp_args,
      runtimeFields.find((f) => f.key === "ubatch")!,
    ),
  );
  if (batch && ubatch > batch)
    throw Error("Physical batch size cannot exceed batch size.");
}
