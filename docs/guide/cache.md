# Manage cache and storage

Use **Cache** to control saved prompt computation and retained weights.
Juiced does not store your client's conversation history.

## Choose a cache mode

| Mode | Behavior |
| --- | --- |
| Off | Disables Juiced's prompt reuse for the model |
| Memory | Reuses supported state while the worker remains alive |
| Persistent | Saves and restores qualified Chat Completions computation snapshots |

1. Open **Cache → Storage policy**.
2. Choose the default mode, retention period, and disk budget.
3. Select **Save changes**.

Changes remain drafts until you save. **Cancel** discards them.

## Override one model

1. Open **Cache → Model overrides**.
2. Select **Add override** and choose a model.
3. Choose its mode.
4. Select **Save changes**.

Only explicit overrides appear in this list. A model using the default stays in
the add chooser. Select **Use default** to remove an override, then save.

Persistent snapshots require a qualified engine/profile and a stable session
identity. Use Memory for Ollama, Anthropic, Responses, and MCP inference when a
persistent deployment rejects their request path. Browser service tests use
memory reuse.

Consumer conversations can outnumber the engine's physical slots. For an observed
llama.cpp slot inventory, active requests wait in Juiced's shared bounded queue;
idle sessions keep no live slot. A qualified persistent session restores its
snapshot when its turn arrives. Queue deadlines and resource pressure can still
return overload errors. Juiced does not shrink the model's context while waiting.

## Connect a client with persistent session reuse

For qualified OpenAI Chat Completions, send the full current message history
with a stable header:

```http
X-Juiced-Session-Id: your-stable-session-id
```

Use a different ID for each client conversation. Session IDs do not authenticate
users. Snapshots bind the authenticated principal, credential epoch, protocol,
session, and execution profile. Key rotation or an incompatible profile causes
a cold rebuild. Clients without usable session affinity retain the memory fallback.

## Retain or forget a snapshot

1. Open **Cache → Snapshots**.
2. Search by model or consumer session.
3. Select a snapshot to inspect its size and reuse details.
4. Use the retention control to keep it, or the forget control to remove it.

Forgetting removes saved computation. The client still owns its messages.
Retained snapshots can prevent automatic reclamation. If the budget cannot
accommodate protected state, inspect retention and free space rather than
assuming that the configured budget guarantees every write will fit.

## Reclaim retained weights

Open **Cache > Retained weights**. Inspect references before reclaiming an unused
copy. Registered or active artifacts can block reclamation. This inventory is
separate from downloaded source weights and prompt snapshots.

Use [backup and restore](maintenance.md) before moving persistent data.
