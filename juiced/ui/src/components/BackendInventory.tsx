import { useSystemInfo, runtimeName, acceleratorName } from "./ExecutionTarget";
import "./BackendInventory.css";
export function BackendInventory() {
  const { info, error } = useSystemInfo();
  const devices = Object.entries(info?.devices || {}).flatMap(([kind, value]) =>
    (Array.isArray(value) ? value : [value])
      .filter((d) => d?.available)
      .map((d) => ({ ...d, kind })),
  );
  return (
    <div className="backend-inventory">
      <header className="page-heading">
        <div>
          <h1>Backends & hardware</h1>
          <p>
            Inspect detected hardware and runtime availability before choosing a
            model's execution settings.
          </p>
        </div>
      </header>
      {error && (
        <p className="inline-error" role="alert">
          {error}
        </p>
      )}
      {!info && !error && <p>Reading runtime inventory…</p>}
      {info && (
        <>
          <section className="hardware-overview" aria-label="Detected hardware">
            {devices.map((d, i) => (
              <article key={i}>
                <strong>
                  {d.kind === "cpu"
                    ? "CPU"
                    : d.kind.includes("npu")
                      ? "NPU"
                      : "GPU"}
                </strong>
                <span>{d.name}</span>
                <small>
                  {d.kind === "cpu"
                    ? `${d.cores} cores · ${d.threads} threads`
                    : d.family ||
                      "Detected; runtime support is a separate check"}
                </small>
              </article>
            ))}
          </section>
          <p className="page-note">
            Detected hardware does not guarantee a compatible runtime or model.
            Installed runtime variants can be selected in a model's Settings.
            Other entries are inventory only; they are not installed
            automatically.
          </p>
          <div className="runtime-inventory-list">
            {Object.entries(info.recipes)
              .sort(
                (a, b) =>
                  Number(
                    Object.values(b[1].backends).some(
                      (v) => v.state === "installed",
                    ),
                  ) -
                    Number(
                      Object.values(a[1].backends).some(
                        (v) => v.state === "installed",
                      ),
                    ) || a[0].localeCompare(b[0]),
              )
              .map(([id, r]) => (
                <section key={id}>
                  <header>
                    <h2>{runtimeName(id, r)}</h2>
                    <span>{r.modality}</span>
                  </header>
                  {Object.entries(r.backends).map(([name, b]) => (
                    <div className="runtime-variant" key={name}>
                      <strong>{acceleratorName(name)}</strong>
                      <span
                        className={b.state === "installed" ? "is-ready" : ""}
                      >
                        {b.state === "installed"
                          ? "Installed"
                          : b.state === "installable"
                            ? "Not installed"
                            : "Unavailable"}
                      </span>
                      <p>
                        {b.message ||
                          (b.state === "installed"
                            ? "Available to compatible model registrations. Model architecture support is checked when loading."
                            : "No ready execution path.")}
                      </p>
                    </div>
                  ))}
                </section>
              ))}
          </div>
        </>
      )}
    </div>
  );
}
