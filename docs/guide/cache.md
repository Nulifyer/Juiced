# Manage the conversation cache

Use **Cache** to control how Juiced reuses processed prompts. When a client sends
the next turn of a conversation, Juiced can give the engine the saved state, so
the engine processes only the new messages. Only llama.cpp models use the cache.
Juiced stores fingerprints of the messages, never their text. Your client keeps
the conversation history.

## Choose a cache mode

| Mode | Behavior |
| --- | --- |
| Off | Juiced does not track conversations. A turn is fast only if the engine still holds that exact prompt. |
| Memory only | Each conversation goes back to the engine slot that still holds it. This needs a model with more than one slot. The state is lost when the model unloads. |
| Memory and disk | Also saves each conversation to disk after a reply, so it comes back after an unload, reload or restart. This is the default. |

1. Open **Cache**.
2. Under **Conversation cache**, select the default mode. The change applies at once.
3. Set **Disk budget** in GiB, from `0.0625` to `128`.
4. Set **Keep unused for** a number of days from 1 to 365, or leave it empty.
5. Select **Save**.

When the disk budget is full, Juiced deletes the least recently used
conversations first. With a number of days, it also deletes conversations unused
for that long; it checks at startup and then once an hour. With the field empty,
conversations stay until space is needed.

## What Juiced saves and deletes

Juiced keeps only saved states that a later request can use.

- A conversation keeps its latest turn. Each save replaces the earlier one.
- One-off requests that open with the same messages keep one saved state, the
  newest. This applies when those messages make up at least nine tenths of each
  request and about 2 KiB or more. The kept state serves that opening to every
  later request. A conversation that continued past its first request is never
  replaced this way.
- When a model starts, Juiced deletes its conversations saved under other engine
  settings, because the engine can no longer restore them. Changed launch
  settings, a Tune result you keep, a new engine build and new weights all change
  the engine settings. While Tune tries settings, nothing is deleted until it
  finishes. Conversations of a model that is only unloaded stay.
- Juiced does not save its own requests: model checks, Tune trials and the test
  prompts Barista sends. At startup it deletes states that earlier versions
  saved for its checks and Tune trials.
- Removing a model deletes its saved conversations.

## Keep or forget a conversation

**Saved conversations** lists the conversations on disk, newest first, by a short
fingerprint.

- Select the pin on a row to keep that conversation. Juiced never deletes a kept
  conversation automatically, but it still counts toward the budget. Select the
  pin again to release it. A kept conversation stays as you pinned it: a later
  turn is saved beside it, a similar request does not replace it, and a change
  of engine settings does not delete it. It can be restored again when the
  earlier settings return.
- Select **Forget** to delete one conversation, or **Forget all** to delete every
  saved conversation, kept ones included. **Forget all** asks first and names the
  count. The next turn then processes the whole prompt again.

**Engine changed** marks a conversation saved under other engine settings. It
cannot be restored. Juiced deletes it when the model starts, unless it is kept.
**Incomplete** marks a save that did not finish.

From a script, `POST /v1/juiced/cache/sessions` takes
`{"action":"retain","session":"<64-hex digest>","retained":true}`. The `configure`
action takes `retention_days` (1 to 365) in `settings`.

## Override one model

1. Under **Model overrides**, choose a model and a mode.
2. Select **Add override**.

Change an override's mode in its row. The change applies at once. Select
**Remove** to return the model to the default mode. A note on the row explains
when the engine cannot use the chosen mode, for example
**This engine doesn't support the conversation cache.**

## Check how a turn used the cache

**Recent turns** shows the last turns sent to llama.cpp models:

- **Still loaded**: the engine still held this conversation or its opening messages.
- **Restored from disk**: Juiced first loaded a saved conversation with the same
  opening messages.
- No badge: the whole prompt was processed, because nothing was cached or the
  client turned caching off with `"cache_prompt": false`.

**Diagnostics → Requests** shows the same detail for every request.

## Start new conversations warm

Most clients send the same long system prompt at the start of every
conversation. Home Assistant, the Barista assistant and character cards are
examples. In **Memory and disk** mode, Juiced remembers the opening messages of
each saved conversation. When a new conversation opens with the same messages,
Juiced restores that saved state first. The engine then processes only the new
messages. This also works after other clients used the slots, and after an
unload or restart.

- Juiced restores a saved state only if the shared opening messages amount to
  about 2 KiB of request text or more. Below that, processing the prompt costs
  less than the restore.
- If several saved states share the same opening messages, Juiced restores the
  smallest one.
- With several slots, Juiced sends a request to the slot that already holds its
  opening messages. If the request's first message changed partway, Juiced reuses
  that client's own slot instead of evicting another client.
- The engine checks the actual tokens, so a restored state never changes the
  reply. Juiced keeps the message fingerprints in `session-lineage.json` next to
  the configuration. The file holds digests, not message text.

Hybrid models such as Qwen3.5 and Qwen3.6 MoE keep recurrent state. They can
reuse a prompt only up to a point where llama.cpp saved a checkpoint, which is
the start of a message or the end of a prompt. If a system prompt changes
partway, for example with the current time, free memory or live device states,
these models process it again from the beginning. Keep changing values out of
the system prompt, or put them in a later message. Barista keeps live figures
out of its system prompt and reads them with `juiced_status`.

## Name a conversation from a client

Chat Completions, Completions, Responses, and the Anthropic, Ollama and MCP
endpoints all use the cache. A client that sends the full message history each
turn needs no extra setup. Juiced matches the conversation from its messages.

To name a conversation, send a stable header:

```http
X-Juiced-Session-Id: your-stable-session-id
```

Juiced also reads `X-Juiced-Session` and `X-Juiced-Thread-Id`. Use a different ID
for each conversation. Session IDs do not authenticate users. A saved
conversation is bound to the API key, protocol, model and engine launch settings
that made it. A different key or a changed setting starts the conversation cold.

## Save a slot by filename

llama.cpp-style clients can save and restore a slot with
`POST /v1/slots/0?action=save` or `?action=restore` and a body like
`{"filename": "story.bin"}`. Juiced keeps the file as a manual snapshot:

1. Set the model's cache mode to **Memory only** or **Off**. In
   **Memory and disk** mode, automatic saves own the slots, and these calls
   return `409`.
2. Use a model with one slot, and turn on **Allow save and restore** under
   **Cache → Manual snapshots**.
3. Send the completion with `X-Juiced-Session: story.bin`, the same value as the
   filename, and the same API key you will save with.
4. Save. Saving the same filename again replaces the earlier snapshot.
5. Restore later with the same key and filename. Another key cannot see it.

`?action=erase` clears the slot. It also needs **Off** mode on a model with
several slots. Add `"model"` to the body, or `?model=`, when several llama.cpp
models are loaded.

## Use manual snapshots

**Manual snapshots** lists snapshots that API clients saved with
`POST /v1/juiced/cache/save`. The switch only allows or blocks manual save and
restore. **Forget** and recovery always work.

1. Choose a model and enter a **Session ID**, or select **New ID**. The console
   does not store the ID, so keep it to restore or forget the snapshot later.
2. Select **Save a snapshot**, enter a **Prompt**, and select **Process prompt**.
3. Select **Save snapshot**.
4. Select **Restore** or **Forget** on a row to use or delete a snapshot.

**Free reserved space** returns the space of a save that did not finish.
**Clear restore protection** releases a snapshot that an earlier restore still
protects. **Check files** checks every saved file, including saved conversations,
for damage and untracked files.

Snapshots can contain sensitive data. Restrict access to the volume and its
backups. See [manage storage](storage.md) for disk use and
[back up and restore](maintenance.md) before you move `/data`.
