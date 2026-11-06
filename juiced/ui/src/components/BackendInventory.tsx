import { useEffect, useMemo, useState } from "react";
import {
  ChevronRight,
  Download,
  ExternalLink,
  RefreshCw,
  Search,
} from "lucide-react";
import {
  backendInstalled,
  backendLabel,
  backendReady,
  backendUpdate,
} from "./backendState";
import { api, type Model } from "../api";
import { useSystemInfo, runtimeName, acceleratorName } from "./ExecutionTarget";
import { HelpTip } from "./HelpTip";
import { Button } from "./t3/button";
import "./BackendInventory.css";

const deviceKind = (kind: string) => kind.split("_").at(-1)!.toUpperCase();

export function BackendInventory({
  jobs = [],
  models,
  openDownloads,
}: {
  jobs?: any[];
  models: Model[];
  openDownloads: () => void;
}) {
  const { info, error, refresh } = useSystemInfo();
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [installing, setInstalling] = useState("");
  const [operationError, setOperationError] = useState("");
  const [notice, setNotice] = useState("");
  const [collapsedMatches, setCollapsedMatches] = useState<Set<string>>(
    new Set(),
  );
  useEffect(() => setCollapsedMatches(new Set()), [filter, query]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>(() => {
    try {
      const saved = JSON.parse(
        localStorage.getItem("juiced.backendGroups") || "{}",
      );
      return saved && typeof saved === "object" && !Array.isArray(saved)
        ? (Object.fromEntries(
            Object.entries(saved).filter(
              ([, value]) => typeof value === "boolean",
            ),
          ) as Record<string, boolean>)
        : {};
    } catch {
      return {};
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem("juiced.backendGroups", JSON.stringify(expanded));
    } catch {
      /* Preferences must not block backend management. */
    }
  }, [expanded]);
  const signature = jobs
    .filter((job) => job.type === "backend" || job.id?.startsWith("backend:"))
    .map((job) => `${job.id}:${job.status}`)
    .join("|");
  useEffect(() => {
    refresh();
  }, [signature, refresh]);
  const devices = Object.entries(info?.devices || {}).flatMap(([kind, value]) =>
    (Array.isArray(value) ? value : [value])
      .filter((device) => device?.available)
      .map((device) => ({ ...device, kind })),
  );
  const rows = useMemo(
    () =>
      Object.entries(info?.recipes || {}).flatMap(([recipe, runtime]) =>
        Object.entries(runtime.backends).map(([backend, variant]) => ({
          recipe,
          runtime,
          backend,
          variant,
        })),
      ),
    [info],
  );
  const category = (state: string) =>
    backendInstalled(state)
      ? "installed"
      : state === "installable"
        ? "available"
        : "unavailable";
  const visible = rows.filter(
    (row) =>
      (filter === "all" ||
        (filter === "updates"
          ? backendUpdate(row.variant.state)
          : category(row.variant.state) === filter)) &&
      `${row.recipe} ${runtimeName(row.recipe, row.runtime)} ${row.backend} ${acceleratorName(row.backend)} ${row.runtime.modality} ${row.variant.devices?.join(" ") || ""}`
        .toLowerCase()
        .includes(query.trim().toLowerCase()),
  );
  const groups = Object.entries(info?.recipes || {})
    .map(([recipe, runtime]) => {
      const all = rows
        .filter((row) => row.recipe === recipe)
        .sort(
          (a, b) =>
            Number(backendReady(b.variant.state)) -
              Number(backendReady(a.variant.state)) ||
            Number(b.variant.state === "installable") -
              Number(a.variant.state === "installable") ||
            acceleratorName(a.backend).localeCompare(
              acceleratorName(b.backend),
            ),
        );

      return {
        recipe,
        runtime,
        all,
        visible: all.filter((row) => visible.includes(row)),
        ready: all.filter((row) => backendReady(row.variant.state)).length,
        loaded: models.filter(
          (model) => model.loaded && model.recipe === recipe,
        ).length,
        updates: all.filter((row) => backendUpdate(row.variant.state)).length,
        active: jobs.some(
          (job) =>
            job.running &&
            all.some(
              (row) =>
                job.id === `backend:${recipe}:${row.backend}` ||
                job.model_name === `${recipe}:${row.backend}`,
            ),
        ),
        failed: jobs.some(
          (job) =>
            job.status === "error" &&
            all.some(
              (row) =>
                job.id === `backend:${recipe}:${row.backend}` ||
                job.model_name === `${recipe}:${row.backend}`,
            ),
        ),
      };
    })
    .filter((group) => group.visible.length)
    .sort(
      (a, b) =>
        Number(b.active || b.failed || !!b.updates) -
          Number(a.active || a.failed || !!a.updates) ||
        Number(b.ready > 0) - Number(a.ready > 0) ||
        b.loaded - a.loaded ||
        runtimeName(a.recipe, a.runtime).localeCompare(
          runtimeName(b.recipe, b.runtime),
        ),
    );
  const filtering = !!query.trim() || filter !== "all";
  async function install(recipe: string, backend: string, update: boolean) {
    setInstalling(`${recipe}:${backend}`);
    setOperationError("");
    setNotice("");
    try {
      const result = await api("install", {
        recipe,
        backend,
        force: update,
        stream: true,
        subscribe: false,
      });
      if (result.action)
        throw Error(
          "This runtime requires manual setup. Use its setup guide, then refresh the inventory.",
        );
      setNotice(
        `Installation of ${runtimeName(recipe)} ${acceleratorName(backend)} is managed by the server. Follow its progress in Downloads.`,
      );
      refresh();
    } catch (error) {
      setOperationError((error as Error).message);
    } finally {
      setInstalling("");
    }
  }
  return (
    <div className="backend-inventory">
      <header className="page-heading">
        <div>
          <div className="heading-with-help"><h1>Backends & hardware</h1><HelpTip label="Backends & hardware">Install and update execution engines here. Select a runtime and device in each model's settings. Hardware detection and an installed package do not establish exact model compatibility.</HelpTip></div>
        </div>
        <Button variant="outline" size="sm" onClick={refresh}>
          <RefreshCw size={14} /> Refresh inventory
        </Button>
      </header>
      {error && (
        <p className="inline-error" role="alert">
          Inventory may be stale: {error}.{" "}
          <Button size="sm" variant="outline" onClick={refresh}>
            Retry inventory
          </Button>
        </p>
      )}
      {operationError && (
        <p className="inline-error" role="alert">
          {operationError}
        </p>
      )}
      {notice && (
        <div className="settings-message" role="status">
          {notice}{" "}
          <Button variant="outline" size="sm" onClick={openDownloads}>
            Open downloads
          </Button>
        </div>
      )}
      {!info && !error && <p role="status">Reading runtime inventory…</p>}
      {info && (
        <>
          <div className="hardware-context">
            <h2>Detected hardware</h2>
            <section
              className="hardware-overview"
              aria-label="Detected hardware"
            >
              {devices.map((device, index) => (
                <article key={index}>
                  <span className="hardware-kind">
                    {deviceKind(device.kind)}
                  </span>
                  <strong>{device.name || device.kind}</strong>
                  <small>
                    {device.kind === "cpu"
                      ? `${device.cores} cores · ${device.threads} threads`
                      : device.family ||
                        "Detected"}
                  </small>
                </article>
              ))}
            </section>
          </div>
          <div
            className="backend-filters"
            role="group"
            aria-label="Backend inventory filter"
          >
            {["all", "installed", "available", "updates", "unavailable"].map(
              (name) => (
                <Button
                  key={name}
                  size="sm"
                  variant={filter === name ? "secondary" : "ghost"}
                  aria-pressed={filter === name}
                  onClick={() => setFilter(name)}
                >
                  {name[0].toUpperCase() + name.slice(1)}{" "}
                  <span>
                    {
                      rows.filter(
                        (row) =>
                          name === "all" ||
                          (name === "updates"
                            ? backendUpdate(row.variant.state)
                            : category(row.variant.state) === name),
                      ).length
                    }
                  </span>
                </Button>
              ),
            )}
          </div>
          <label className="search-input backend-search">
            <Search size={15} />
            <input
              aria-label="Search backends"
              placeholder="Find a runtime, backend or capability…"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
          <p className="backend-result-count" role="status">
            {groups.length} provider{groups.length === 1 ? "" : "s"} ·{" "}
            {visible.length} execution variant{visible.length === 1 ? "" : "s"}
            {filtering ? " match" : ""}
          </p>
          <div className="backend-table">
            {groups.map((group) => {
              const open = filtering
                ? !collapsedMatches.has(group.recipe)
                : (expanded[group.recipe] ??
                  (group.loaded > 0 ||
                    group.active ||
                    group.failed ||
                    group.updates > 0));
              return (
                <section
                  className="backend-group"
                  key={group.recipe}
                  aria-label={`${runtimeName(group.recipe, group.runtime)} provider`}
                >
                  <h2>
                    <button
                      className="backend-group-toggle"
                      aria-expanded={open}
                      aria-controls={`backend-variants-${group.recipe}`}
                      onClick={() => {
                        if (filtering)
                          setCollapsedMatches((current) => {
                            const next = new Set(current);
                            if (open) next.add(group.recipe);
                            else next.delete(group.recipe);
                            return next;
                          });
                        else
                          setExpanded((current) => ({
                            ...current,
                            [group.recipe]: !open,
                          }));
                      }}
                    >
                      <ChevronRight size={16} aria-hidden="true" />
                      <span className="backend-group-title">
                        <strong>
                          {runtimeName(group.recipe, group.runtime)}
                        </strong>
                        <small>{group.runtime.modality}</small>
                      </span>
                      <span className="backend-group-summary">
                        <span>
                          {group.ready} / {group.all.length} ready
                          {filtering ? ` · ${group.visible.length} shown` : ""}
                        </span>
                        {group.loaded > 0 && (
                          <small>
                            {group.loaded} model{group.loaded === 1 ? "" : "s"}{" "}
                            loaded
                          </small>
                        )}
                        {group.active && <small>Installing</small>}
                        {group.failed && (
                          <small className="backend-job-error">
                            Installation needs attention
                          </small>
                        )}
                        {group.updates > 0 && (
                          <small>
                            {group.updates} update
                            {group.updates === 1 ? "" : "s"}
                          </small>
                        )}
                      </span>
                    </button>
                  </h2>
                  {!open && (
                    <p className="backend-group-variant-names">
                      {group.all
                        .map((row) => acceleratorName(row.backend))
                        .join(" · ")}
                    </p>
                  )}
                  <div
                    className="backend-group-variants"
                    id={`backend-variants-${group.recipe}`}
                    hidden={!open}
                  >
                    {group.visible.map(({ recipe, backend, variant }) => {
                      const id = `${recipe}:${backend}`;
                      const job = jobs.find(
                        (job) =>
                          job.id === `backend:${id}` || job.model_name === id,
                      );
                      const used = models.some(
                        (model) => model.loaded && model.recipe === recipe,
                      );
                      const updating = backendUpdate(variant.state);
                      const setup =
                        variant.action?.match(/https:\/\/[^\s"<>]+/)?.[0];
                      return (
                        <article className="backend-entry" key={id}>
                          <div className="backend-entry-identity">
                            <strong>{acceleratorName(backend)}</strong>
                            <small>
                              {variant.version || "—"}
                            </small>
                          </div>
                          <div className="backend-entry-state">
                            <strong
                              className={
                                backendReady(variant.state) ? "is-ready" : ""
                              }
                            >
                              {backendLabel(variant.state)}
                            </strong>
                            {variant.message && <HelpTip label={`${runtimeName(recipe)} ${acceleratorName(backend)}`}>{variant.message}</HelpTip>}
                            {updating && used && (
                              <small>
                                Unload workers using this runtime before
                                updating.
                              </small>
                            )}
                            {job && (
                              <small>
                                {job.running
                                  ? "Installation in progress"
                                  : job.status}
                                {job.error ? `: ${job.error}` : ""}
                              </small>
                            )}
                          </div>
                          <div className="backend-entry-action">
                            {job?.running ? (
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={openDownloads}
                              >
                                View progress
                              </Button>
                            ) : setup ? (
                              <a href={setup} target="_blank" rel="noreferrer">
                                Setup guide <ExternalLink size={12} />
                              </a>
                            ) : variant.state === "installable" || updating ? (
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={!!installing || (updating && used)}
                                title={
                                  updating && used
                                    ? "Unload workers using this runtime before updating its backend."
                                    : undefined
                                }
                                onClick={() =>
                                  void install(recipe, backend, updating)
                                }
                              >
                                <Download size={14} />
                                {installing === id
                                  ? "Starting…"
                                  : updating
                                    ? "Update backend"
                                    : "Install backend"}
                              </Button>
                            ) : null}
                          </div>
                        </article>
                      );
                    })}
                  </div>
                </section>
              );
            })}
          </div>
          {!visible.length && (
            <p className="backend-empty">
              No backends match this filter. Change the filter or search to see
              the inventory.
            </p>
          )}
        </>
      )}
    </div>
  );
}
