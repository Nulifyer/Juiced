import { useEffect, useRef, useState } from "react";
import { Trash2 } from "lucide-react";
import { api, type Model } from "../api";
import { Button } from "./t3/button";
import "./ModelRemoval.css";

export function ModelRemoval({
  model,
  focusRequested,
  onRemoved,
  onRefresh,
  notify,
}: {
  model: Model;
  focusRequested?: boolean;
  onRemoved: () => Promise<void>;
  onRefresh: () => Promise<void>;
  notify: (message: string) => void;
}) {
  const section = useRef<HTMLElement>(null);
  useEffect(() => {
    if (focusRequested) section.current?.scrollIntoView({ block: "center" });
  }, [focusRequested, model.id]);
  const [plan, setPlan] = useState<any>();
  const [error, setError] = useState("");
  const [review, setReview] = useState(false);
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    setPlan(undefined);
    setError("");
    setReview(false);
    setConfirmation("");
    void api(
      "juiced/removal?model=" + encodeURIComponent(model.id),
      undefined,
      controller.signal,
    )
      .then((next) => {
        setPlan(next);
        if (focusRequested && next.allowed) setReview(true);
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [model.id, model.loaded, model.policy, model.operation?.pending, focusRequested]);
  async function unload() {
    setBusy(true);
    setError("");
    try {
      await api("juiced/operation", { model: model.id, action: "unload" });
      await onRefresh();
      notify("Model unload requested.");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function remove() {
    setBusy(true);
    setError("");
    try {
      await api("juiced/removal", {
        model: model.id,
        expected: plan,
        confirmation,
      });
      await onRemoved();
      notify(
        plan.action === "unregister"
          ? "Model registration removed. Source files preserved."
          : "Downloaded model weights removed.",
      );
    } catch (e) {
      setError((e as Error).message);
      setReview(false);
      setConfirmation("");
      try {
        setPlan(
          await api("juiced/removal?model=" + encodeURIComponent(model.id)),
        );
      } catch {
        setPlan(undefined);
      }
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="model-removal" ref={section}>
      <h2>Remove model</h2>
      <p>{plan?.description || "Checking model storage ownership…"}</p>
      {plan?.reason && <p className="removal-reason">{plan.reason}</p>}
      {model.loaded && <div className="removal-unload">
        <Button size="compact" variant="outline" onClick={() => void unload()}
          disabled={busy || model.policy === "hot" || model.runtime?.is_busy || model.operation?.pending}
          title={model.policy === "hot" ? "Change loading policy above before unloading a Hot model" : undefined}>
          {model.operation?.pending ? "Unloading…" : "Unload model"}
        </Button>
        {model.policy === "hot" && <p>Set loading policy above to On demand or Disabled, then unload before removing.</p>}
      </div>}
      {error && (
        <p role="alert" className="inline-error">
          {error}
        </p>
      )}
      {!review ? (
        <Button
          variant="destructive-outline"
          size="compact"
          disabled={!plan?.allowed || busy}
          onClick={() => setReview(true)}
        >
          <Trash2 size={14} />
          {plan?.label || "Review removal"}…
        </Button>
      ) : (
        <div className="removal-confirmation">
          <p>
            Confirm removal of <strong>{model.id}</strong>. Model caches and
            client history are managed separately.
          </p>
          <label>
            Type the model name to confirm
            <input
              aria-label="Confirm model removal"
              autoComplete="off"
              spellCheck={false}
              value={confirmation}
              onChange={(e) => setConfirmation(e.target.value)}
            />
          </label>
          <div>
            <Button
              variant="destructive"
              disabled={busy || confirmation !== model.id}
              onClick={() => void remove()}
            >
              {busy ? "Removing…" : plan.label}
            </Button>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => {
                setReview(false);
                setConfirmation("");
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}
