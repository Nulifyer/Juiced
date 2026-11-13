---
topic: connect-client
version: 1
title: Connect a client application
summary: Give the endpoint, protocol, model IDs and an API key for clients such as SillyTavern, Pi, Open WebUI or Home Assistant.
---
# Connect a client application

1. `juiced_connections`: base URL, enabled protocols with health, model IDs and aliases, and whether keys are required.
2. Pick the protocol the client speaks:
   - OpenAI-compatible (SillyTavern Chat Completion, Open WebUI, Pi, most apps): base URL `<base>/v1`.
   - Ollama clients: `<base>` with the Ollama protocol enabled.
   - Anthropic clients: `<base>` with `POST /v1/messages`.
   - Home Assistant voice: Wyoming, see `juiced_help` topic `setup-voice`.
3. A disabled protocol: `juiced_set_protocol` to enable it, then `juiced_connections` again to confirm it is healthy.
4. If keys are required (mode `keys`): `juiced_create_api_key` with a clear `name` and the narrowest `scope` (`inference` for apps). The secret is never shown to you. Tell the operator to select **Reveal key** in the chat and copy it into the client now; it is shown once. In mode `trusted-lan` the first key must be created from Connect, because it turns on keys for every route including the console.
5. Give a short checklist: base URL, model ID (or alias), key location. Offer `juiced_set_alias` for a stable name the client can keep when the model changes.
