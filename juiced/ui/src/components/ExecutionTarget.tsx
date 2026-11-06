import { useCallback, useEffect, useState } from "react";
import { backendReady } from "./backendState";
import { HelpTip } from "./HelpTip";
import { api, type Model } from "../api";
import { SelectField, type SelectOption } from "./t3/select";
import { runtimeFields, setArg, type Options } from "./modelSettings";

export type RuntimeInfo = {
  display_name: string;
  modality: string;
  default_backend?: string;
  options?: { name: string; type_name: string; help: string }[];
  backends: Record<
    string,
    {
      state: string;
      message?: string;
      devices?: string[];
      version?: string;
      action?: string;
      update_available?: boolean;
    }
  >;
};
export type SystemInfo = {
  recipes: Record<string, RuntimeInfo>;
  devices: Record<string, any>;
};
export const runtimeName = (id: string, info?: RuntimeInfo) =>
  id === "llamacpp"
    ? "llama.cpp"
    : id === "vllm"
      ? "vLLM"
      : info?.display_name || id;
export const acceleratorName = (name: string) =>
  ({
    cpu: "CPU",
    vulkan: "GPU · Vulkan",
    rocm: "GPU · ROCm",
    cuda: "GPU · CUDA",
    metal: "GPU · Metal",
    npu: "NPU",
    system: "System runtime",
  })[name] || name;
export function useSystemInfo() {
  const [info, setInfo] = useState<SystemInfo>();
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const refresh = useCallback(() => setRevision((value) => value + 1), []);
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    void api<SystemInfo>("system-info", undefined, controller.signal)
      .then(setInfo)
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [revision]);
  return { info, error, refresh };
}
export function ExecutionTarget({
  model,
  effective,
  draft,
  update,
  disabled,
}: {
  model: Model;
  effective: Options;
  draft: Options;
  update: (key: string, value: any) => void;
  disabled?: boolean;
}) {
  const { info, error } = useSystemInfo();
  const recipe = info?.recipes[model.recipe];
  const backendKey = Object.keys(effective).find((k) => k.endsWith("_backend"));
  const backend = String(
    backendKey
      ? (draft[backendKey] ??
          effective[backendKey] ??
          recipe?.default_backend ??
          "")
      : recipe?.default_backend || Object.keys(recipe?.backends || {})[0] || "",
  );
  const supported = recipe?.backends[backend];
  const device = Object.prototype.hasOwnProperty.call(draft, "llamacpp_device")
    ? String(draft.llamacpp_device || "")
    : String(effective.llamacpp_device || "");
  const [devices, setDevices] = useState<{ id: string; name: string }[]>([]);
  const [deviceError, setDeviceError] = useState("");
  useEffect(() => {
    setDevices([]);
    setDeviceError("");
    if (
      model.recipe !== "llamacpp" ||
      backend === "cpu" ||
      !backendReady(supported?.state || "")
    )
      return;
    const controller = new AbortController();
    void api(
      `juiced/devices?recipe=${encodeURIComponent(model.recipe)}&backend=${encodeURIComponent(backend)}`,
      undefined,
      controller.signal,
    )
      .then((result) => setDevices(result.devices || []))
      .catch((e) => {
        if (!controller.signal.aborted) setDeviceError(e.message);
      });
    return () => controller.abort();
  }, [model.recipe, backend, supported?.state]);
  const deviceNames = (supported?.devices || []).flatMap((id) => {
    const d = info?.devices[id];
    return (Array.isArray(d) ? d : [d])
      .filter((d) => d?.available)
      .map((d) => d.name);
  });
  const alternatives = Object.entries(info?.recipes || {}).filter(
    ([id, r]) => id !== model.recipe && r.modality === recipe?.modality,
  );
  function choose(value: string) {
    if (!backendKey) return;
    try {
      const nextArgs =
        model.recipe === "llamacpp"
          ? setArg(
              String(draft.llamacpp_args ?? effective.llamacpp_args ?? ""),
              runtimeFields.find((f) => f.key === "gpu")!,
              value === "cpu" ? "0" : "999",
            )
          : undefined;
      update(backendKey, value);
      if (nextArgs !== undefined) {
        update("llamacpp_device", null);
        update("llamacpp_args", nextArgs);
      }
    } catch (e) {
      setDeviceError((e as Error).message);
    }
  }
  const backendOptions: SelectOption[] = recipe
    ? Object.entries(recipe.backends).map(([value, variant]) => ({
        value,
        label: acceleratorName(value),
        disabled: !backendReady(variant.state),
        description: backendReady(variant.state)
          ? undefined
          : variant.state === "installable"
            ? "Not installed"
            : "Unavailable",
      }))
    : [{ value: backend, label: backend || "Reading runtime inventory…" }];
  if (recipe && !recipe.backends.npu)
    backendOptions.push({
      value: "unavailable-npu",
      label: "NPU",
      description: "No compatible runtime path",
      disabled: true,
    });
  backendOptions.push({
    value: "unavailable-hybrid",
    label: "Hybrid NPU + GPU",
    description: "Not implemented by this runtime",
    disabled: true,
  });
  return (
    <fieldset className="settings-section execution-target" disabled={disabled}>
      <legend>Runtime & hardware <HelpTip label="Runtime compatibility">Execution choices belong to this model registration. Installed packages alone do not prove compatibility with its architecture or quantization. Reload to apply a changed execution target.</HelpTip></legend>
      {error && (
        <p className="inline-error" role="alert">
          Hardware inventory unavailable: {error}. No new hardware choices are
          assumed.
        </p>
      )}
      <div className="settings-grid">
        <div className="registered-runtime-field">
          <span className="field-label">Model runtime</span>
          <div className="registered-runtime">
            {runtimeName(model.recipe, recipe)}
          </div>

        </div>
        <label htmlFor="execution-backend">
          <span className="field-label">Run on</span>
          <SelectField
            id="execution-backend"
            label="Run on"
            value={backend}
            options={backendOptions}
            disabled={disabled || !recipe || !backendKey}
            onValueChange={choose}
          />
          <small>
            {deviceNames.join(", ") ||
              supported?.message ||
              (!recipe
                ? "Reading the server's detected hardware…"
                : "No selectable acceleration variant for this registration.")}
          </small>
        </label>
      </div>
      {model.recipe === "llamacpp" && backend !== "cpu" && (
        <label className="execution-device" htmlFor="execution-device">
          <span className="field-label">Accelerator device <HelpTip label="Accelerator device">Device IDs come from the selected runtime. Automatic selection lets the runtime choose a GPU.</HelpTip></span>
          <SelectField
            id="execution-device"
            label="Accelerator device"
            value={device}
            disabled={disabled}
            onValueChange={(next) => update("llamacpp_device", next || null)}
            options={[
              { value: "", label: "Automatic GPU selection" },
              ...devices.map((item) => ({
                value: item.id,
                label: `${item.id} · ${item.name}`,
              })),
              ...(device && !devices.some((item) => item.id === device)
                ? [
                    {
                      value: device,
                      label: device,
                      description: "Current explicit selection",
                    },
                  ]
                : []),
            ]}
          />
          {deviceError && <small role="alert">{deviceError}</small>}
        </label>
      )}
      <div className="execution-alternatives"><HelpTip label="Other execution paths">
        <ul className="execution-reasons">
          {Object.entries(recipe?.backends || {})
            .filter(([, b]) => !backendReady(b.state))
            .map(([id, b]) => (
              <li key={id}>
                <strong>{acceleratorName(id)}</strong>
                <span>
                  {b.message ||
                    (b.state === "installable"
                      ? "Supported by this host, but the runtime package is not installed."
                      : "Not supported in this runtime environment.")}
                </span>
              </li>
            ))}
          {alternatives.map(([id, r]) => (
            <li key={id}>
              <strong>{runtimeName(id, r)}</strong>
              <span>
                {Object.values(r.backends).some((b) => backendReady(b.state))
                  ? "Installed for other registrations."
                  : Object.values(r.backends).some(
                        (b) => b.state === "installable",
                      )
                    ? "Runtime package is not installed."
                    : "Unavailable in the current runtime environment."}{" "}
                This model is registered with a different runtime. Exact artifact compatibility has not been established here.
              </span>
            </li>
          ))}
          <li>
            <strong>Hybrid NPU + GPU</strong>
            <span>
              The current model runtime does not implement this execution path.
              Selecting two devices cannot enable it.
            </span>
          </li>
        </ul>
      </HelpTip><span>Other execution paths</span></div>
    </fieldset>
  );
}
