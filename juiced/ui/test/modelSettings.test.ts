import { expect, test } from "bun:test";
import {
  editableOptions,
  joinArgs,
  optionPatch,
  readArg,
  runtimeFields,
  samplingFields,
  setArg,
  splitArgs,
  stripArgs,
  validateSettings,
  withExtraArgs,
} from "../src/components/modelSettings";
const threads = runtimeFields.find((f) => f.key === "threads")!;
const source = `--host 127.0.0.1 -t 8 --parallel 2 --chat-template-kwargs '{"enable_thinking":true}' --slot-save-path '/data/prompt state' --cache-ram 4096 --spec-type none`;
test("editing a normal control preserves template JSON, cache paths and unknown flags", () => {
  const changed = setArg(source, threads, "12");
  expect(readArg(changed, threads)).toBe("12");
  expect(splitArgs(stripArgs(changed, [threads]))).toEqual(
    splitArgs(stripArgs(source, [threads])),
  );
  expect(splitArgs(changed)).toContain('{"enable_thinking":true}');
  expect(splitArgs(changed)).toContain("/data/prompt state");
});
test("canonical edits replace duplicate and equals aliases and retain negative values", () => {
  const changed = setArg(
    "--threads=4 -t 8 --reasoning-budget -1 --parallel=2",
    threads,
    "6",
  );
  expect(readArg(changed, threads)).toBe("6");
  expect(splitArgs(changed).filter((t) => t === "-t")).toHaveLength(1);
  expect(
    readArg(
      changed,
      samplingFields.find((f) => f.key === "reasoning")!,
    ),
  ).toBe("-1");
});
test("argument round trips preserve escaped quotes, literal dollars, spaces and empty strings", () => {
  const args = ["--template", `a'b "c" $HOME`, "", "--path", "a\\b"];
  expect(splitArgs(joinArgs(args))).toEqual(args);
  expect(splitArgs('--path "C:\\models\\cache"')).toEqual([
    "--path",
    "C:\\models\\cache",
  ]);
  expect(() => splitArgs("--foo 'unfinished")).toThrow();
});
test("advanced arguments do not overwrite form values and maintain unrepresented flags", () => {
  const changed = withExtraArgs(
    source,
    `--spec-type none --chat-template-kwargs '{"enable_thinking":false}'`,
  );
  expect(readArg(changed, threads)).toBe("8");
  expect(readArg(changed, runtimeFields[0])).toBe("2");
  expect(splitArgs(changed)).toContain('{"enable_thinking":false}');
  expect(() => withExtraArgs(source, "--threads=4")).toThrow();
});
test("patches preserve unrelated options and never submit the policy-owned pin state", () => {
  const original = {
    pinned: true,
    ctx_size: 131072,
    llamacpp_args: source,
    llamacpp_device: "Vulkan0",
  };
  const next = { ...editableOptions(original), ctx_size: 65536 };
  expect(optionPatch(editableOptions(original), next)).toEqual({
    ctx_size: 65536,
  });
  expect(optionPatch(original, { ...next, llamacpp_device: null })).toEqual({
    ctx_size: 65536,
    llamacpp_device: null,
  });
});
test("reject invalid capacities and sampling without silently truncating values", () => {
  for (const ctx_size of [0, -2, 1.5])
    expect(() => validateSettings({ ctx_size })).toThrow();
  for (const llamacpp_args of [
    "--parallel 0",
    "--top-p 1.5",
    "-b 64 -ub 128",
    "-t 2.5",
  ])
    expect(() => validateSettings({ llamacpp_args })).toThrow();
  expect(() =>
    validateSettings({
      ctx_size: -1,
      llamacpp_args: "--reasoning-budget -1 --parallel 2 -b 2048 -ub 512",
    }),
  ).not.toThrow();
});
