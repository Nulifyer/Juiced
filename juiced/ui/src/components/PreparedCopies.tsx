import { useEffect, useState } from "react";
import { api, bytes } from "../api";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
type Artifact = {
  id: string;
  bytes: number;
  quant: string;
  models: string[];
  removable: boolean;
};
export function PreparedCopies() {
  const [items, setItems] = useState<Artifact[]>([]),
    [error, setError] = useState("");
  const [confirm, setConfirm] = useState(""),
    [busy, setBusy] = useState(false);
  async function refresh() {
    try {
      const result = await api<{ artifacts: Artifact[] }>(
        "juiced/preparation?cache=1",
      );
      setItems(result.artifacts);
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }
  useEffect(() => {
    void refresh();
  }, []);
  async function remove(id: string) {
    setBusy(true);
    try {
      await api("juiced/preparation", { action: "delete_cache", id });
      setConfirm("");
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="prepared-copies" aria-label="Prepared copies">
      <div className="profile-title">
        <h3>Prepared copies</h3>
        <HelpTip label="Prepared copies">
          Converted weights are separate from computation snapshots and original
          downloads. Copies in use by a model registration are protected. Remove
          the registration from Models before reclaiming its cached copy.
        </HelpTip>
        <Button variant="ghost" size="sm" onClick={() => void refresh()}>
          Refresh
        </Button>
      </div>
      {error && (
        <p className="inline-error" role="alert">
          {error}
        </p>
      )}
      {!items.length && !error && (
        <p className="page-note">No locally prepared weights.</p>
      )}
      {items.map((item) => (
        <article className="prepared-copy" key={item.id}>
          <div>
            <strong>
              {item.models.join(", ") || item.quant + " · unused copy"}
            </strong>
            <small>
              {bytes(item.bytes)} · {item.id.slice(0, 12)}
            </small>
          </div>
          {item.removable ? (
            <div className="profile-actions">
              {confirm === item.id ? (
                <>
                  <Button
                    disabled={busy}
                    variant="destructive"
                    onClick={() => void remove(item.id)}
                  >
                    Delete cached weights
                  </Button>
                  <Button variant="ghost" onClick={() => setConfirm("")}>
                    Cancel
                  </Button>
                </>
              ) : (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setConfirm(item.id)}
                >
                  Reclaim space
                </Button>
              )}
            </div>
          ) : (
            <span className="muted">In use</span>
          )}
        </article>
      ))}
    </section>
  );
}
