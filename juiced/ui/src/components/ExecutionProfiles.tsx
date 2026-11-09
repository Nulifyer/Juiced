import { useEffect, useState } from "react";
import { Dialog } from "@base-ui/react/dialog";
import { api, type Model } from "../api";
import { Button } from "./t3/button";
import { SelectField } from "./t3/select";
import { HelpTip } from "./HelpTip";
import {
  acceleratorHelp,
  acceleratorName,
  runtimeHelp,
  runtimeName,
} from "./runtimeInfo";
import "./ExecutionProfiles.css";

type Target = {
  original_backend?: string;
  settings?: Record<string, any>;
  tokenizer?: string;
  id: string;
  recipe: string;
  backend?: string;
  compatible: boolean;
  available: boolean;
  reason?: string;
  hardware_reason?: string;
  package_state?: string;
  experimental?: boolean;
  requires_tokenizer?: boolean;
  changes: string[];
};
type Plan = {
  model: string;
  selected: string;
  architecture: string;
  profiles: Target[];
  activity?: {
    npu_matmul_nodes: number;
    prefill_plans: number;
    npu_planned_weight_bytes: number;
    npu_copy_budget_bytes: number;
  };
  activity_error?: string;
};
type Job = {
  id: string;
  model: string;
  name: string;
  phase: string;
  pending: boolean;
  error?: string;
  cached?: boolean;
};
export function ExecutionProfiles({
  model,
  dirty,
  refresh,
  onChanged,
  onFindVariant,
}: {
  model: Model;
  dirty: boolean;
  refresh: () => Promise<void>;
  onChanged: () => void;
  onFindVariant: (runtime: string) => void;
}) {
  const [plan, setPlan] = useState<Plan>();
  const [targetId, setTargetId] = useState(""),
    [acknowledge, setAcknowledge] = useState(false);
  const [hybridMemory, setHybridMemory] = useState(2048),
    [hybridPrompt, setHybridPrompt] = useState(1024);
  const [tokenizer, setTokenizer] = useState(""),
    [device, setDevice] = useState("");
  const [devices, setDevices] = useState<{ id: string; name: string }[]>([]),
    [deviceError, setDeviceError] = useState("");
  const [output, setOutput] = useState<string | null>(null);
  const [jobs, setJobs] = useState<Job[]>([]),
    [error, setError] = useState("");
  const [busy, setBusy] = useState(false),
    [message, setMessage] = useState(""),
    [revision, setRevision] = useState(0);
  const pending = busy || model.operation?.pending;
  useEffect(() => {
    const controller = new AbortController();
    setPlan(undefined);
    setError("");
    void api<Plan>(
      `juiced/execution?model=${encodeURIComponent(model.id)}`,
      undefined,
      controller.signal,
    ).then((execution) => {
        if (controller.signal.aborted) return;
        setPlan(execution);
        setTargetId(execution.selected);
        setAcknowledge(false);
        setError("");
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [model.id, revision, model.operation?.pending]);
  useEffect(() => {
    const controller = new AbortController();
    const poll = () =>
      void api<{ jobs: Record<string, Job> }>(
        "juiced/preparation",
        undefined,
        controller.signal,
      )
        .then((result) =>
          setJobs(
            Object.values(result.jobs)
              .filter((job) => job.model === model.id)
              .reverse(),
          ),
        )
        .catch((e) => {
          if (!controller.signal.aborted) setError(e.message);
        });
    poll();
    const timer = window.setInterval(poll, 1500);
    return () => {
      controller.abort();
      window.clearInterval(timer);
    };
  }, [model.id]);
  const target = plan?.profiles.find((t) => t.id === targetId);
  const runtimes = [...new Set(plan?.profiles.map((t) => t.recipe) || [])];
  const variants =
    plan?.profiles.filter((t) => t.recipe === target?.recipe) || [];
  const targetHelp = (t: Target) =>
    [
      t.id === "registered"
        ? "The model's original registered setup and saved settings."
        : undefined,
      acceleratorHelp(t.backend || t.original_backend || ""),
      t.reason,
      !t.available ? t.hardware_reason : undefined,
    ].filter(Boolean).join(" ");
  useEffect(() => {
    const controller = new AbortController();
    setDevices([]);
    setDeviceError("");
    setDevice(String(target?.settings?.llamacpp_device || ""));
    setHybridMemory(
      Number(target?.settings?.llamacpp_hybrid_memory_mib || 2048),
    );
    setHybridPrompt(
      Number(target?.settings?.llamacpp_hybrid_min_prompt || 1024),
    );
    setTokenizer(target?.tokenizer || "");
    if (
      target?.recipe === "llamacpp" &&
      target.backend &&
      target.backend !== "cpu" &&
      target.available &&
      target.package_state !== "installable"
    )
      void api<{ devices: { id: string; name: string }[] }>(
        `juiced/devices?recipe=llamacpp&backend=${encodeURIComponent(target.backend)}`,
        undefined,
        controller.signal,
      )
        .then((result) =>
          setDevices(
            result.devices.filter(
              (d) =>
                target.backend !== "hybrid-rocm" || d.id.startsWith("ROCm"),
            ),
          ),
        )
        .catch((e) => {
          if (!controller.signal.aborted) setDeviceError(e.message);
        });
    return () => controller.abort();
  }, [targetId, plan]);
  function choose(id: string) {
    setTargetId(id);
    setAcknowledge(false);
    setMessage("");
  }
  async function select() {
    if (!plan || !target) return;
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await api("juiced/execution", {
        model: model.id,
        profile: targetId,
        expected_selected: plan.selected,
        acknowledge,
        tokenizer,
        device,
        hybrid_memory_mib: hybridMemory,
        hybrid_min_prompt: hybridPrompt,
        reload: model.loaded,
      });
      setMessage(
        model.loaded
          ? "Switching runtime. A failed load restores the previous profile."
          : "Saving the execution profile for the next load.",
      );
      await refresh();
      onChanged();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function showOutput(id: string) {
    try {
      const result = await api<{ output?: string }>(
        `juiced/preparation?id=${encodeURIComponent(id)}`,
      );
      setOutput(result.output || "No retained output.");
    } catch (e) {
      setError((e as Error).message);
    }
  }
  const risks = !!target && (target.experimental || target.changes.length > 0);
  return (
    <div className="execution-profiles">
      <section className="profile-section" aria-label="Execution profile">
        <div className="profile-title">
          <h3>Runtime & hardware</h3>
          <HelpTip label="Execution profiles">
            Reuse compatible weights. Each profile retains settings. A failed
            switch restores the previous profile. Experimental profiles still
            need your output and performance testing.
          </HelpTip>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => setRevision((v) => v + 1)}
          >
            Refresh
          </Button>
        </div>
        {!plan && !error && <p role="status">Reading compatible runtimes…</p>}
        {plan && (
          <>
            <div className="settings-grid">
              <label>
                <span className="field-label">
                  Runtime
                  <HelpTip label="Runtime">
                    {runtimeHelp(target?.recipe || "llamacpp")}
                  </HelpTip>
                </span>
                <SelectField
                  label="Runtime"
                  value={target?.recipe || ""}
                  options={runtimes.map((recipe) => ({
                    value: recipe,
                    label: runtimeName(recipe),
                    tooltip: runtimeHelp(recipe),
                  }))}
                  onValueChange={(recipe) =>
                    choose(
                      plan.profiles.find(
                        (t) => t.recipe === recipe && t.id !== "registered",
                      )?.id || "registered",
                    )
                  }
                  disabled={pending}
                />
              </label>
              <label>
                <span className="field-label">
                  Run on
                  <HelpTip label="Run on">
                    {target ? targetHelp(target) : "Choose a runtime, then a compatible hardware profile."}
                  </HelpTip>
                </span>
                <SelectField
                  label="Run on"
                  value={targetId}
                  options={variants.map((t) => ({
                    value: t.id,
                    tooltip: targetHelp(t),
                    label:
                      t.id === "registered"
                        ? t.original_backend
                          ? `${acceleratorName(t.original_backend)} · Original`
                          : "Original setup"
                        : acceleratorName(t.backend || ""),
                    description: !t.compatible
                      ? "Incompatible"
                      : !t.available
                        ? "Unavailable"
                        : t.experimental
                          ? "Experimental"
                          : t.package_state === "installable"
                            ? "Install on first load"
                            : undefined,
                  }))}
                  onValueChange={choose}
                  disabled={pending}
                />
              </label>
            </div>
            {target && (
              <>
                {(!target.compatible || !target.available) && (
                  <p className="inline-error">
                    {!target.compatible ? target.reason : target.hardware_reason}
                  </p>
                )}
                {target.requires_tokenizer && (
                  <label>
                    <span className="field-label">
                      Original tokenizer / config repository
                      <HelpTip label="Tokenizer and config">
                        Use the exact original repository or a local directory
                        with its tokenizer and config.
                      </HelpTip>
                    </span>
                    <input
                      aria-label="Original tokenizer / config repository"
                      value={tokenizer}
                      onChange={(e) => setTokenizer(e.target.value)}
                      placeholder="publisher/model"
                    />
                  </label>
                )}
                {target.recipe === "llamacpp" &&
                  target.backend &&
                  target.backend !== "cpu" && (
                    <label>
                      <span className="field-label">
                        Accelerator device
                        <HelpTip label="Accelerator device">
                          Choose which GPU the runtime uses. Automatic uses the
                          runtime's default GPU. In hybrid mode this selects the
                          ROCm GPU; the NPU is paired separately.
                        </HelpTip>
                      </span>
                      <SelectField
                        label="Accelerator device"
                        value={device}
                        options={[
                          {
                            value: "",
                            label: "Automatic GPU selection",
                            tooltip: "Let the selected runtime choose its default GPU.",
                          },
                          ...devices.map((d) => ({
                            value: d.id,
                            label: `${d.id} · ${d.name}`,
                            tooltip: `Use ${d.name} through ${d.id}. This chooses a device, not a different model or quant.`,
                          })),
                        ]}
                        onValueChange={setDevice}
                        disabled={pending}
                      />
                      {deviceError && <small role="alert">{deviceError}</small>}
                    </label>
                  )}
                {target.backend === "hybrid-rocm" && (
                  <div className="settings-grid">
                    <label>
                      <span className="field-label">
                        NPU copy budget (MiB)
                        <HelpTip label="NPU copy budget">
                          Bounds additional NPU weight copies. Keep room for the
                          GPU model, context, other Hot models and XRT working
                          buffers.
                        </HelpTip>
                      </span>
                      <input
                        type="number"
                        aria-label="NPU copy budget (MiB)"
                        min="128"
                        max="16384"
                        value={hybridMemory}
                        onChange={(e) =>
                          setHybridMemory(Number(e.target.value))
                        }
                      />
                    </label>
                    <label>
                      <span className="field-label">
                        Minimum prefill rows
                        <HelpTip label="Minimum prefill batch">Applies to each chunk, not the total prompt length. This bundle defaults to 2,048 rows per chunk. A higher threshold prevents NPU dispatch unless you also increase microbatch size in Advanced.</HelpTip>
                      </span>
                      <input
                        type="number"
                        aria-label="Minimum prefill rows"
                        min="1024"
                        max="32768"
                        value={hybridPrompt}
                        onChange={(e) =>
                          setHybridPrompt(Number(e.target.value))
                        }
                      />
                    </label>
                  </div>
                )}
                {risks && (
                  <div className="profile-warning">
                    <strong>Experimental profile</strong>
                    {target.changes.map((change) => (
                      <p key={change}>{change}</p>
                    ))}
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={acknowledge}
                        onChange={(e) => setAcknowledge(e.target.checked)}
                      />
                      I’ve reviewed these changes
                    </label>
                  </div>
                )}
                {dirty && (
                  <p className="inline-error">
                    Save or revert your settings before changing runtime.
                  </p>
                )}
                <div className="profile-actions">
                  <Button
                    disabled={
                      pending ||
                      dirty ||
                      !model.downloaded ||
                      !target.compatible ||
                      !target.available ||
                      (risks && !acknowledge) ||
                      (!!target.requires_tokenizer && !tokenizer)
                    }
                    onClick={() => void select()}
                  >
                    {model.loaded ? "Apply & reload" : "Use this profile"}
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={() => onFindVariant(target.recipe)}
                  >
                    Find a published variant
                  </Button>
                </div>
                {plan.activity && (
                  <div className="profile-activity" aria-label="Current worker activity">
                    <span className="field-label">
                      Current worker
                      <HelpTip label="Actual NPU activity">
                        Cumulative work in this worker; reload resets it. Zero work
                        can mean the prompt is below the prefill threshold.
                        Selecting a hybrid profile alone does not prove NPU
                        acceleration. Use Refresh after a test.
                      </HelpTip>
                    </span>
                    <dl>
                      <div><dt>NPU matmuls</dt><dd>{plan.activity.npu_matmul_nodes.toLocaleString()}</dd></div>
                      <div><dt>Prefill plans</dt><dd>{plan.activity.prefill_plans.toLocaleString()}</dd></div>
                      <div>
                        <dt>Weight copies</dt>
                        <dd>{Math.round(plan.activity.npu_planned_weight_bytes / 1048576)} / {Math.round(plan.activity.npu_copy_budget_bytes / 1048576)} MiB</dd>
                      </div>
                    </dl>
                    {!plan.activity.npu_matmul_nodes && <small>No NPU prefill recorded.</small>}
                  </div>
                )}
                {plan.activity_error && <small role="status">{plan.activity_error}</small>}
                <small className="profile-current">
                  Saved profile:{" "}
                  {plan.selected === "registered"
                    ? "Original setup"
                    : plan.selected}{" "}
                  {plan.architecture && `· ${plan.architecture}`}
                </small>
              </>
            )}
          </>
        )}
      </section>
      {jobs.length > 0 && <section className="profile-section" aria-label="Previous conversion jobs">
        <div className="profile-title"><h3>Previous conversion jobs</h3>
          <HelpTip label="Previous conversion jobs">Juiced no longer creates converted or quantized copies. Existing records and unresolved cleanup remain available. Cancellation is a request, not proof that a writer stopped.</HelpTip>
        </div>
        {jobs.length > 0 && (
          <div className="preparation-jobs" aria-label="Preparation jobs">
            {jobs.slice(0, 12).map((job) => (
              <article key={job.id}>
                <div>
                  <strong>{job.name}</strong>
                  <small role="status">
                    {job.phase}
                    {job.cached ? " · reused cached output" : ""}
                    {job.phase === "complete" ? " · historical completion" : ""}
                  </small>
                  {job.error && (
                    <small className="inline-error">{job.error}</small>
                  )}
                </div>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void showOutput(job.id)}
                >
                  Output
                </Button>
                {job.pending && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() =>
                      void api("juiced/preparation", {
                        action: "cancel",
                        id: job.id,
                      }).catch((e) => setError(e.message))
                    }
                  >
                    Cancel
                  </Button>
                )}
              </article>
            ))}
          </div>
        )}
      </section>}
      <Dialog.Root
        open={output !== null}
        onOpenChange={(open) => {
          if (!open) setOutput(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Backdrop className="modal-backdrop" />
          <Dialog.Popup className="modal preparation-output">
            <Dialog.Title>Previous conversion output</Dialog.Title>
            <pre>{output}</pre>
            <Dialog.Close render={<Button variant="outline" />}>
              Close
            </Dialog.Close>
          </Dialog.Popup>
        </Dialog.Portal>
      </Dialog.Root>
      {error && (
        <p className="inline-error" role="alert">
          {error}
        </p>
      )}
      {model.operation?.error && (
        <p className="inline-error" role="alert">
          {model.operation.error}
        </p>
      )}
      {message && (
        <p role="status" className="profile-reason">
          {message}
        </p>
      )}
    </div>
  );
}
