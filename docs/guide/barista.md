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

The chat lives in the browser tab and clears when you close it. **Clear** starts
over. The panel shows the model, the permission and what Barista is doing.

The settings button in the panel changes the model or permissions, or turns
Barista off. Turning it off stops the current request and refuses later tool
calls. Changes that already finished stay in place. A download or engine
install it started keeps running under its own job, and shared model workers
keep running.

Barista works only from the console. Applications that call the OpenAI or MCP
APIs get ordinary inference, never Barista's tools.
