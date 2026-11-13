import { describe, expect, test } from "bun:test";
import { loadInventory, parseRustRoutes, registrationLines, validateInventory, validateMainComparison } from "./inventory";

describe("native API preservation inventory", () => {
  test("the frozen native registrations and current Rust declarations validate", async () => {
    const inventory = await loadInventory();
    await validateInventory(inventory);
    expect(inventory.summary.native_http_routes).toBe(135);
    expect(inventory.summary.native_websocket_routes).toBe(4);
    expect(inventory.summary.rust_published_routes).toBe(149);
    expect(inventory.summary.rust_native_same_path_rows).toBe(84);
    expect(inventory.summary.native_http_status_counts.implemented).toBe(0);
  });

  test("a missing native route fails even when all Rust routes remain inventoried", async () => {
    const inventory = await loadInventory();
    inventory.http_routes = inventory.http_routes.filter(route => route.canonical_path !== "/registry/search");
    expect(inventory.rust_published_routes.length).toBe(149);
    await expect(validateInventory(inventory)).rejects.toThrow("Registration references missing route");
  });

  test("omitting a finite dynamic registration site cannot pass closure", async () => {
    const inventory = await loadInventory();
    inventory.closure.registration_sites = inventory.closure.registration_sites.filter(site => !(site.path.endsWith("juiced/service.cpp") && site.line === 869));
    await expect(validateInventory(inventory)).rejects.toThrow("Native registration sites changed");
  });

  test("source and protocol prefix drift require an explicit review", async () => {
    const sourceDrift = await loadInventory();
    sourceDrift.sources.find(source => source.path === "src/cpp/server/server.cpp")!.sha256 = "0".repeat(64);
    await expect(validateInventory(sourceDrift)).rejects.toThrow("Native source drift");
    const prefixDrift = await loadInventory();
    prefixDrift.prefix_schemes.fixed = ["/v1"];
    await expect(validateInventory(prefixDrift)).rejects.toThrow("Protocol exceptions must retain fixed paths");
  });

  test("a matching Rust handler cannot silently become a wire-parity claim", async () => {
    const inventory = await loadInventory();
    inventory.http_routes.find(route => route.method === "POST" && route.canonical_path === "/delete")!.status = "implemented";
    await expect(validateInventory(inventory)).rejects.toThrow("native differential receipt");
  });

  test("mixed preparation controls preserve cleanup while excluding producers", async () => {
    const inventory = await loadInventory();
    const preparation = inventory.http_routes.find(route => route.method === "POST" && route.canonical_path === "/juiced/preparation")!;
    expect(preparation.status).toBe("planned");
    expect(preparation.subsets!.find(subset => subset.status === "excluded")!.reason).toContain("conversion");
    expect(preparation.subsets!.find(subset => subset.status === "planned")!.reason).toContain("writer ownership");
    const nativeDelete = inventory.http_routes.find(route => route.method === "POST" && route.canonical_path === "/delete")!;
    expect(nativeDelete.semantic_boundary).toContain("retaining weights");
  });

  test("registration scanning observes helper, parameterized and mount statements", () => {
    const source = 'void routes() {\n  register_post("jobs", handler);\n  http.Get(prefix + "juiced/" + operation, handler);\n  http.set_mount_point("/", ui);\n}';
    expect(registrationLines(source, 1, 5)).toEqual([2, 3, 4]);
    const rust = parseRustRoutes('(get, "/health", health, "read"),', 'Endpoint { method: "POST", path: "/mcp", authority: "inference" }');
    expect(rust).toEqual([
      { method: "GET", path: "/health", prefix_scheme: "native_quad", authority: "read" },
      { method: "POST", path: "/mcp", prefix_scheme: "fixed", authority: "inference" },
    ]);
  });

  test("captured main observations cannot authorize missing native additions", async () => {
    const snapshot = await Bun.file(new URL("main-comparison.json", import.meta.url)).json();
    validateMainComparison(snapshot);
    snapshot.conclusion.new_http_routes.push("/unreviewed");
    expect(() => validateMainComparison(snapshot)).toThrow("route addition review");
  });
});
