# Connect Home Assistant speech

Use Juiced's optional private Wyoming listener for transcription and speech
synthesis through the same managed workers.

## Prepare the speech models

1. Download a supported transcription model in Juiced.
2. Download a Kokoro speech model.
3. Enable both models and complete Transcribe and Speak tests.
4. Choose Hot policies if these services should remain ready for voice requests.

The current listener advertises enabled transcription models and Kokoro speech
models. It provides English PCM speech. Model-specific language or voice support
still applies.

## Enable the listener

Add this environment entry to the Juiced container:

```yaml
    environment:
      JUICED_WYOMING_ENABLED: "true"
```

The default listener binds `tcp://0.0.0.0:10300` inside the container.
`JUICED_WYOMING_URI` can set a different bind URI.
If Home Assistant shares the private Docker network, use the Juiced service name.
Otherwise, publish TCP port `10300` only on a private interface reachable by Home
Assistant. Recreate the container after changing deployment settings.

Wyoming has no protocol authentication. Restrict network access to trusted clients.
The HTTP reverse proxy and HTTP API keys do not protect this TCP listener.

## Configure Home Assistant

1. Open Home Assistant's integrations settings.
2. Add a **Wyoming Protocol** integration.
3. Enter Juiced's reachable private host and TCP port.
4. Choose the advertised transcription and speech services in your voice pipeline.
5. Send a real voice turn and check both recognized text and spoken output.

Use **Service APIs → Wyoming** to inspect listener status and advertised support.
A running helper process does not prove a completed voice turn.
Voice changes can require a listener restart before Home Assistant sees them.

## Connect a conversation model separately

Wyoming provides speech transport. Configure the conversation integration against
Juiced's [OpenAI-compatible](../api/openai.md) or [Ollama-compatible](../api/ollama.md)
interface when supported by the chosen Home Assistant integration.
Use an enabled chat model, its exact ID, and a compatible cache mode.
The conversation integration owns Home Assistant tools and device control.
