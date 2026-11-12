import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { isAbsolute, resolve } from "node:path";

type Evidence = { path: string; line: number; text: string; role?: string };
type RouteRef = { method: string; path: string; prefix_scheme: string };
type Route = {
  id: string;
  method: string;
  canonical_path: string;
  prefix_scheme: string;
  category: string;
  native_behavior: string;
  native_authority: string;
  disposition: string;
  status: string;
  native_evidence: Evidence[];
  plan_feature_ids: string[];
  documentation?: string;
  semantic_boundary: string;
  rust_equivalent: {
    relationship: string;
    routes: RouteRef[];
    scenario_files: string[];
    authorities: { method: string; path: string; authority: string }[];
  };
  subsets?: { status: string; feature_ids: string[]; reason: string; evidence?: Evidence[] }[];
};
type RustRoute = RouteRef & {
  authority: string;
  evidence: Evidence;
  native_route_ids: string[];
  native_same_path: boolean;
  scenario_files: string[];
};
type RegistrationSite = Evidence & { classification: string; route_ids: string[] };
export type Inventory = {
  schema_version: number;
  main_comparison_file: string;
  sources: { path: string; sha256: string; hash_check: string }[];
  prefix_schemes: Record<string, string[]>;
  status_definitions: Record<string, string>;
  closure: {
    status: string;
    unresolved_dynamic_registrations: unknown[];
    scopes: { path: string; start_line: number; end_line: number }[];
    registration_sites: RegistrationSite[];
    native_activation: Evidence[];
  };
  authority_contracts: Record<string, { evidence: Evidence[]; boundary: string }>;
  plan_evidence: Record<string, { feature: string; scope: string; phase: string; disposition: string; evidence: Evidence }>;
  decision_evidence: Evidence[];
  http_routes: Route[];
  websocket_routes: Route[];
  auxiliary_http_routes: Route[];
  rust_published_routes: RustRoute[];
  summary: {
    native_http_routes: number;
    native_websocket_routes: number;
    auxiliary_http_route_families: number;
    native_registration_sites: number;
    rust_published_routes: number;
    rust_native_same_path_rows: number;
    native_http_status_counts: Record<string, number>;
  };
};

export const repositoryRoot = resolve(import.meta.dir, "../../..");
const nativeHttp = "juiced/rewrite/service/src/http.rs";
const compatibility = "juiced/rewrite/service/src/compat.rs";
const registrationPattern = /\b(?:register_get|register_post|post|remove)\(|\b(?:web_server|http|server)\.(?:Get|Post|Delete|Options|set_mount_point)\(/;
const statuses = ["implemented", "partial", "planned", "excluded", "conditional"];

function requireValue(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function normalized(path: string): string {
  return path.replace(/\{[^}]+\}/g, "{param}");
}

function routeKey(route: RouteRef): string {
  return `${route.method} ${route.prefix_scheme} ${normalized(route.path)}`;
}

function equalSets(actual: Iterable<string>, expected: Iterable<string>, message: string) {
  const left = new Set(actual);
  const right = new Set(expected);
  const added = [...left].filter(value => !right.has(value));
  const missing = [...right].filter(value => !left.has(value));
  requireValue(!added.length && !missing.length, `${message}; added=${added.join(",")}; missing=${missing.join(",")}`);
}

export function registrationLines(source: string, start: number, end: number): number[] {
  return source.split("\n").flatMap((line, index) =>
    index + 1 >= start && index + 1 <= end && registrationPattern.test(line) ? [index + 1] : []);
}

export function parseRustRoutes(native: string, compat: string): (RouteRef & { authority: string })[] {
  const routes: (RouteRef & { authority: string })[] = [];
  for (const match of native.matchAll(/\((get|post), "([^"]+)", (\w+), "([^"]+)"\)/g)) {
    routes.push({ method: match[1]!.toUpperCase(), path: match[2]!, prefix_scheme: "native_quad", authority: match[4]! });
  }
  for (const match of compat.matchAll(/Endpoint \{\s*method: "([^"]+)",\s*path: "([^"]+)",\s*authority: "([^"]+)"/g)) {
    routes.push({ method: match[1]!, path: match[2]!, prefix_scheme: "fixed", authority: match[3]! });
  }
  return routes;
}

export async function loadInventory(): Promise<Inventory> {
  return JSON.parse(await readFile(resolve(import.meta.dir, "inventory.json"), "utf8")) as Inventory;
}

export async function validateInventory(inventory: Inventory, root = repositoryRoot): Promise<string[]> {
  requireValue(inventory.schema_version === 1, "Unknown API parity inventory schema");
  equalSets(Object.keys(inventory.status_definitions), statuses, "Status definitions changed");
  requireValue(inventory.closure.status === "closed_for_hashed_registration_owners", "Native registration closure is unresolved");
  requireValue(inventory.closure.unresolved_dynamic_registrations.length === 0, "Unresolved native dynamic registrations remain");
  requireValue(JSON.stringify(inventory.prefix_schemes.native_quad) === JSON.stringify(["/api/v0", "/api/v1", "/v0", "/v1"]), "Native quad prefixes changed");
  requireValue(JSON.stringify(inventory.prefix_schemes.fixed) === '[""]', "Protocol exceptions must retain fixed paths");
  requireValue(JSON.stringify(inventory.prefix_schemes.bare_and_native_quad) === JSON.stringify(["", "/api/v0", "/api/v1", "/v0", "/v1"]), "WebSocket path aliases changed");

  const sourceText = new Map<string, string>();
  const warnings: string[] = [];
  for (const source of inventory.sources) {
    requireValue(!isAbsolute(source.path) && !source.path.split(/[\\/]/).includes(".."), "Source paths must stay inside the repository");
    requireValue(!sourceText.has(source.path), `Duplicate source ${source.path}`);
    requireValue(/^[a-f0-9]{64}$/.test(source.sha256), `Invalid source digest ${source.path}`);
    requireValue(["required", "snapshot_only"].includes(source.hash_check), `Unknown source hash policy ${source.path}`);
    const bytes = await readFile(resolve(root, source.path));
    sourceText.set(source.path, bytes.toString("utf8"));
    const unchanged = createHash("sha256").update(bytes).digest("hex") === source.sha256;
    if (source.hash_check === "required") requireValue(unchanged, `Native source drift requires route review: ${source.path}`);
    else if (!unchanged) warnings.push(`Snapshot source changed; route and evidence checks still apply: ${source.path}`);
  }

  function evidence(reference: Evidence) {
    const source = sourceText.get(reference.path);
    requireValue(source !== undefined, `Evidence source is not inventoried: ${reference.path}`);
    requireValue(Number.isInteger(reference.line) && reference.line > 0 && reference.text.trim(), "Invalid evidence locator");
    requireValue(source.split("\n")[reference.line - 1]?.trim() === reference.text, `Evidence anchor drift: ${reference.path}:${reference.line}`);
  }

  const features = JSON.parse(sourceText.get("juiced/PLATFORM-FEATURES.json")!);
  const featureIds = new Set<string>(features.entries.map((feature: { id: string }) => feature.id));
  for (const [id, reference] of Object.entries(inventory.plan_evidence)) {
    const current = features.entries.find((feature: { id: string }) => feature.id === id);
    requireValue(current && ["feature", "scope", "phase", "disposition"].every(key => current[key] === reference[key as keyof typeof reference]), `Approved feature disposition changed: ${id}`);
    evidence(reference.evidence);
  }
  for (const reference of inventory.decision_evidence) evidence(reference);
  const routes = [...inventory.http_routes, ...inventory.websocket_routes, ...inventory.auxiliary_http_routes];
  const routeIds = new Set<string>();
  const rustKeys = new Set(inventory.rust_published_routes.map(routeKey));
  requireValue(rustKeys.size === inventory.rust_published_routes.length, "Duplicate Rust route");
  for (const route of routes) {
    requireValue(!routeIds.has(route.id), `Duplicate native route ${route.id}`);
    routeIds.add(route.id);
    requireValue(["GET", "POST", "DELETE", "OPTIONS"].includes(route.method), `Unknown native method ${route.id}`);
    requireValue(route.canonical_path.startsWith("/") && inventory.prefix_schemes[route.prefix_scheme], `Invalid native path/prefix ${route.id}`);
    requireValue(statuses.includes(route.status) && route.semantic_boundary.trim(), `Missing native status/boundary ${route.id}`);
    requireValue(route.native_authority && route.native_evidence.length && route.plan_feature_ids.length, `Missing authority/evidence/disposition ${route.id}`);
    requireValue(route.plan_feature_ids.every(id => featureIds.has(id) && inventory.plan_evidence[id]), `Unknown plan feature ${route.id}`);
    requireValue(route.rust_equivalent.relationship !== "none" || !route.rust_equivalent.routes.length, `Unexplained Rust route reference ${route.id}`);
    requireValue(route.status !== "partial" || route.rust_equivalent.relationship !== "none", `Partial route lacks current Rust evidence ${route.id}`);
    requireValue(route.status !== "implemented", `An implemented claim needs a separately reviewed native differential receipt: ${route.id}`);
    for (const reference of route.native_evidence) evidence(reference);
    for (const subset of route.subsets ?? []) {
      requireValue(statuses.includes(subset.status) && subset.reason && subset.feature_ids.every(id => featureIds.has(id) && inventory.plan_evidence[id]), `Invalid mixed endpoint subset ${route.id}`);
      for (const reference of subset.evidence ?? []) evidence(reference);
    }
    if (route.status === "excluded") {
      requireValue(route.disposition === "excluded" && ["explicit_501", "explicit_410"].includes(route.native_behavior) && route.plan_feature_ids.some(id => inventory.plan_evidence[id]?.scope === "excluded"), `Excluded native route lacks an approved rejection boundary ${route.id}`);
    }
    for (const reference of route.rust_equivalent.routes) {
      requireValue(reference.prefix_scheme === "framework" || rustKeys.has(routeKey(reference)), `Dangling Rust route equivalent ${route.id}`);
    }
    for (const path of route.rust_equivalent.scenario_files) requireValue(sourceText.has(path) && path.startsWith("juiced/rewrite/rust-tests/tests/"), `Missing actual-process scenario file ${path}`);
    if (route.documentation) requireValue(sourceText.has(route.documentation), `Missing native documentation ${route.documentation}`);
  }

  const siteKeys = new Set<string>();
  const linkedRoutes = new Set<string>();
  for (const site of inventory.closure.registration_sites) {
    const key = `${site.path}:${site.line}`;
    requireValue(!siteKeys.has(key), `Duplicate registration site ${key}`);
    siteKeys.add(key);
    evidence(site);
    requireValue(["routes", "registration_helper"].includes(site.classification), `Unchecked native registration site ${key}`);
    requireValue(site.classification !== "routes" || site.route_ids.length, `Native registration emits no inventoried route ${key}`);
    requireValue(site.classification !== "registration_helper" || !site.route_ids.length, `Registration helper has unexplained route bindings ${key}`);
    for (const id of site.route_ids) {
      requireValue(routeIds.has(id), `Registration references missing route ${id}`);
      linkedRoutes.add(id);
    }
  }
  const observedSites = new Set<string>();
  for (const scope of inventory.closure.scopes) {
    const source = sourceText.get(scope.path);
    requireValue(source && inventory.sources.some(file => file.path === scope.path && file.hash_check === "required"), `Registration owner is not hash-checked: ${scope.path}`);
    for (const line of registrationLines(source, scope.start_line, scope.end_line)) observedSites.add(`${scope.path}:${line}`);
  }
  equalSets(observedSites, siteKeys, "Native registration sites changed without an inventory review");
  for (const route of [...inventory.http_routes, ...inventory.auxiliary_http_routes]) requireValue(linkedRoutes.has(route.id), `Native route lacks a registration binding ${route.id}`);
  for (const reference of inventory.closure.native_activation) evidence(reference);
  for (const owner of Object.values(inventory.authority_contracts)) for (const reference of owner.evidence) evidence(reference);

  const currentRust = parseRustRoutes(sourceText.get(nativeHttp)!, sourceText.get(compatibility)!);
  equalSets(currentRust.map(routeKey), rustKeys, "Rust published route set changed without native parity review");
  for (const route of inventory.rust_published_routes) {
    evidence(route.evidence);
    requireValue(currentRust.some(current => routeKey(current) === routeKey(route) && current.authority === route.authority), `Rust route authority changed ${route.path}`);
    requireValue(route.native_route_ids.every(id => routeIds.has(id)), `Dangling Rust/native cross-reference ${route.path}`);
    const samePath = inventory.http_routes.some(native => native.rust_equivalent.relationship === "same_path" && native.method === route.method && normalized(native.canonical_path) === normalized(route.path));
    requireValue(route.native_same_path === samePath, `Incorrect native match count ${route.path}`);
  }
  const summary = inventory.summary;
  requireValue(summary.native_http_routes === inventory.http_routes.length, "Native HTTP count mismatch");
  requireValue(summary.native_websocket_routes === inventory.websocket_routes.length, "Native WebSocket count mismatch");
  requireValue(summary.auxiliary_http_route_families === inventory.auxiliary_http_routes.length, "Auxiliary HTTP count mismatch");
  requireValue(summary.native_registration_sites === siteKeys.size, "Native registration count mismatch");
  requireValue(summary.rust_published_routes === rustKeys.size, "Rust published count mismatch");
  requireValue(summary.rust_native_same_path_rows === inventory.rust_published_routes.filter(route => route.native_same_path).length, "Rust native-match count mismatch");
  for (const status of statuses) requireValue(summary.native_http_status_counts[status] === inventory.http_routes.filter(route => route.status === status).length, `Native status count mismatch: ${status}`);
  requireValue(inventory.main_comparison_file === "juiced/rewrite/api-parity/main-comparison.json", "Unexpected main comparison snapshot path");
  validateMainComparison(JSON.parse(await readFile(resolve(root, inventory.main_comparison_file), "utf8")));
  return warnings;
}

export function validateMainComparison(snapshot: any): void {
  requireValue(snapshot.schema_version === 1 && /^[a-f0-9]{40}$/.test(snapshot.main_source_head), "Invalid main comparison snapshot");
  requireValue(snapshot.conclusion.route_set_equal === true && snapshot.conclusion.new_http_routes.length === 0 && snapshot.conclusion.new_websocket_routes.length === 0, "Main comparison requires a separate route addition review");
  for (const source of snapshot.source_comparisons) {
    requireValue(!isAbsolute(source.path) && !source.path.split(/[\\/]/).includes("..") && /^[a-f0-9]{64}$/.test(source.main_sha256), "Invalid main snapshot source label/digest");
    requireValue(source.validation === "snapshot_only_no_main_path_access_in_ci", "Main source must remain a read-only captured observation");
    requireValue(source.present_in_frozen || source.frozen_sha256 === null, "Main-only source must not require a frozen file");
  }
  for (const scope of snapshot.registration_scope_comparisons) requireValue(scope.equal === true && scope.frozen_scope_sha256 === scope.main_scope_sha256, "Native main registration scope comparison is unresolved");
  for (const observation of snapshot.semantic_observations) {
    requireValue(observation.boundary && observation.evidence.length, "Missing main semantic boundary");
    for (const reference of observation.evidence) requireValue(reference.authority === "main_snapshot_only" && reference.path && reference.line > 0 && reference.text, "Invalid captured main evidence");
  }
}

if (import.meta.main) {
  requireValue(JSON.stringify(process.argv.slice(2)) === '["--check"]', "Usage: bun inventory.ts --check");
  const inventory = await loadInventory();
  const warnings = await validateInventory(inventory);
  for (const warning of warnings) process.stderr.write(`${warning}\n`);
  process.stdout.write(`${JSON.stringify(inventory.summary)}\n`);
}
