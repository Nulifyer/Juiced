# Use Barista

Barista is an optional assistant in the console. You ask it to check or change
Juiced in plain words, and it uses one of your own chat models to do the work
through the same checks as the console.

Barista is off until an operator turns it on. Turning it on downloads nothing
and loads nothing by itself.

## Turn it on

1. Connect the console with an operator key, or from a trusted network when no
   keys are set.
2. Select **Enable Barista** in the top bar. **Not now** hides the invitation in
   this browser and changes nothing on the server. The **Barista** button stays
   in the top bar for later.
3. Pick a downloaded chat model. The setup says how it loads. A Disabled model
   is set to On demand when you turn Barista on. Its other settings stay as
   they are.
4. Pick the permissions and select **Turn on Barista**.

| Permission | What Barista does |
| --- | --- |
| Full control | Makes changes without asking. Each change shows in the chat. |
| Ask before changes | Waits for you to approve or skip each change. |
| Read only | Looks at Juiced but cannot change it. |

No model has been tested for Barista yet, so check what it reports. Barista
cannot change its own permissions, and every change still passes the normal
revision checks.

## Chat and settings

Once Barista is on, the **Barista** button opens the chat directly. The setup
appears again only when you select the settings button, or when the chosen
model is no longer registered.

The chat lives in the browser tab and clears when you close it. **Clear** starts
over. The panel shows the model, the permission and what Barista is doing.
Replies render as Markdown. Each tool Barista runs shows as one short line, and
its reasoning folds into a **Thought for Ns** line that you can expand. When
Barista's model accepts images, you can paste, drop or pick a screenshot.

On wide screens, the pin button docks Barista beside the page instead of over
it. This browser remembers whether Barista is pinned and open. On narrow
screens Barista always opens over the page.

The settings button in the panel changes the model or permissions, or turns
Barista off. Turning it off stops the current request and refuses later tool
calls. Changes that already finished stay in place. A download or engine
install it started keeps running under its own job, and shared model workers
keep running.

## What Barista can do

Barista uses the same checks as the console and never runs shell commands or
touches files outside Juiced. It can:

| Group | Examples |
| --- | --- |
| Inspect | Status and alerts, hardware and usable backends, models, engines, recent requests, logs, cache, connections |
| Acquire | Search Hugging Face, check a repository's quants and license, estimate fit, download, add a model, install an engine |
| Configure | Loading policy, load and unload, context and slots, cache mode, protocols, aliases, Wyoming, API keys |
| Verify | Send one real test request (chat, embeddings, speech, transcription, image, rerank) or run the quick speed benchmark |
| Maintain | Remove a model's registration, forget saved sessions, uninstall an unused engine, pause requests, restart a model |
| Explain | Step-by-step procedures for whole jobs, and the docs bundled with this version |

Some operations are not available yet. Barista says so when you ask for them:
the storage breakdown and deleting unused model files, deleting files when a
model is removed, and sampling, KV cache, batch, flash attention and extra
argument settings.

Barista follows shipped procedures for common jobs, such as setting up Home
Assistant voice, finding and running a model that fits, changing a model's
context, finding why a request was slow, freeing disk space, connecting a client
and checking the cache. It checks its work with a test request or the model's
reported settings before it says a job is done.

### What the chat shows

- Each check Barista makes is one short line. A test request shows its result,
  including an audio player or image when there is one.
- Each change shows what it changed, before and after.
- A download or engine install shows its progress, and keeps updating after
  Barista finishes its reply.
- When Barista creates an API key, select **Reveal key** to see it once and copy
  it. Barista itself never sees the key. The first key, which turns on keys for
  every route, must be created from **Connect**.

Barista cannot disable, remove or switch off the protocol for the model it runs
on, and it cannot change its own permissions. Hot models load when Juiced starts,
so Barista loads a model it just made Hot if you want it running now.

Barista works only from the console. Applications that call the OpenAI or MCP
APIs get ordinary inference, never Barista's tools.
