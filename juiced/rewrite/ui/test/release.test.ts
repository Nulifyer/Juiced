import { describe, expect, test } from "bun:test";
import { parseRelease, releaseLabel } from "../src/release";

describe("release notice", () => {
  test("shows a newer release with its GitHub page", () => {
    expect(parseRelease({ check: true, current: "0.10.0", latest: "v0.10.1", url: "https://github.com/Nulifyer/Juiced/releases/tag/v0.10.1", newer: true, checked_at_ms: 5 }))
      .toEqual({ check: true, current: "0.10.0", latest: "v0.10.1", url: "https://github.com/Nulifyer/Juiced/releases/tag/v0.10.1", newer: true });
    expect(releaseLabel("v0.10.1")).toBe("v0.10.1 available");
    expect(releaseLabel("0.11.0")).toBe("v0.11.0 available");
  });
  test("never links outside the project's releases and stays quiet without a version", () => {
    expect(parseRelease({ check: true, current: "0.10.0", latest: "v0.10.1", url: "https://evil.example/x", newer: true })?.url).toBe("https://github.com/Nulifyer/Juiced/releases");
    expect(parseRelease({ check: true, current: "0.10.0", latest: "<b>", url: null, newer: true })).toMatchObject({ latest: null, newer: false, url: null });
    expect(parseRelease({ check: false, current: "0.10.0", latest: null, url: null, newer: false })).toMatchObject({ check: false, newer: false });
    expect(parseRelease(undefined)).toBeNull();
    expect(parseRelease({ check: "yes" })).toBeNull();
  });
});
