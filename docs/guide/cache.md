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
- One-off requests that open with the same text keep one saved state, the
  newest. This applies when the shared text makes up at least nine tenths of
  each request and 1 KiB or more. The kept state serves that opening to every
  later request. A conversation that continued past its first request is never
  replaced this way.
- A request sent with `X-Juiced-Cache: no-store` is not saved. See
  [skip saving one-off requests](#skip-saving-one-off-requests).
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

- **Still loaded**: the engine still held the start of this prompt.
- **Restored from disk**: Juiced first loaded a saved conversation that starts
  the same way.
- No badge: the engine read the whole prompt, because nothing was cached or the
  client turned caching off with `"cache_prompt": false`.

The badge reports what the engine did. A turn counts as **Still loaded** when
the engine reused the same conversation, or at least 64 tokens of any prompt it
still held. Fewer reused tokens are the template header that every prompt of a
model shares, so that turn has no badge.

**Diagnostics → Requests** shows the same detail for every request.

## Start new conversations warm

Most clients send the same long text at the start of every request. Home
Assistant, the Barista assistant and character cards send a system prompt.
Other clients send one long document and then a question. In
**Memory and disk** mode, Juiced remembers how each saved conversation starts.
When a new request starts with the same text, Juiced restores that saved state
first. The engine then reads only the text that follows. This also works after
other clients used the slots, and after an unload or restart.

- The shared text can end anywhere, also inside a message. A system prompt that
  ends with the date, or a document followed by a different question, reuses
  everything before the first difference. Text that differs from its first
  line shares nothing.
- Juiced restores a saved state only if the shared text is 1 KiB or more, about
  250 tokens, and reading it again would take at least twice as long as the
  restore. Juiced measures both on each model. In a test a restore took about
  50 ms plus 0.3 ms per MiB of state: 0.06 seconds for 14 MiB and 0.38 seconds
  for 1.1 GiB. The CPU engine in that test reads 22 to 55 tokens in that time.
  An engine that reads 5,000 tokens a second needs about 2 KiB of shared text
  before the smallest restore pays.
- A saved state of the same conversation is always restored.
- If several saved states share the same text, Juiced restores the smallest
  one.
- With several slots, Juiced sends a request to the slot that already holds its
  opening text. A request that failed or was cancelled keeps its slot, so a
  retry continues where the engine stopped.
- The engine checks the actual tokens, so a restored state never changes the
  reply. Juiced keeps the fingerprints in `session-lineage.json` next to the
  configuration. The file holds digests, not message text. Conversations saved
  by an earlier version match by whole messages until their next save.

Hybrid models such as Qwen3.5 and Qwen3.6 MoE keep recurrent state. They can
reuse a prompt only up to a point where llama.cpp saved a checkpoint, which is
the start of a message or the end of a prompt. If a system prompt changes
partway, for example with the current time, free memory or live device states,
these models process it again from the beginning. Keep changing values out of
the system prompt, or put them in a later message. Barista keeps live figures
out of its system prompt and reads them with `juiced_status`.

## Fit long conversations in the budget

A saved state grows with the conversation. A 43,000-token conversation on
Qwen3-Coder-Next takes about 2.25 GiB. Each model may use 4 GiB of the disk
budget, so two conversations of that length do not both stay saved. The one
used less recently is deleted, and its next turn reads the whole prompt again.

To alternate between long conversations on one model, raise the model's share
of the budget. `POST /v1/juiced/cache/sessions` with the `configure` action
takes `model_budget_bytes` in `settings`, up to `budget_bytes`. While Juiced
saves a state it reserves a quarter more than the state's expected size, so two
conversations of 2.25 GiB need 5.2 GiB. A model with two slots is another way:
each slot keeps one conversation in memory.

A conversation larger than the model's share is never saved. A kept
conversation holds its space, so a full budget of kept conversations saves
nothing new.

## Skip saving one-off requests

A client that sends requests nothing will continue, such as a batch job or a
title generator, can mark them:

```http
X-Juiced-Cache: no-store
```

The request still reuses what the engine holds and can restore a saved state.
Juiced does not save its own state, so it writes nothing to disk and the next
request does not wait for a save. In a test, four one-off requests of 1,600
tokens sent back to back took 22.9 seconds and wrote 734 MiB, because three of
them waited for the save before them. With the header they took 17.8 seconds
and wrote nothing. Any other value returns `400 invalid_cache_directive`.

`"cache_prompt": false` in the request body is stricter: the engine reads the
whole prompt and Juiced neither restores nor saves.

## Name a conversation from a client

Chat Completions, Completions, Responses, and the Anthropic, Ollama and MCP
endpoints all use the cache. A client that sends the full message history each
turn needs no extra setup. Juiced matches the conversation from its messages.

To name a conversation, send a stable header:

```http
X-Juiced-Session-Id: your-stable-session-id
```

Juiced also reads `X-Juiced-Session` and `X-Juiced-Thread-Id`. Use a different ID
for each conversation. A new ID starts from another named conversation of the
same client when both start with the same text. Session IDs do not authenticate users. A saved
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
