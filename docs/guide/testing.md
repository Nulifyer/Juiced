# Test model services

Use **Test services** to check a loaded model before connecting another app.
Enable the model and wait for its worker to load first.

## Test chat

1. Open **Test services → Try a model**.
2. Choose a chat model. One that is not loaded loads on the first message.
3. Send a short prompt and check that the answer completes.
4. Use **Stop** to cancel the request.

**Max tokens** caps each reply, up to the model's output limit. Turn **Thinking**
off to ask a reasoning model to answer directly; other models ignore it.

Replies render as Markdown: tables, lists, links and code blocks with a copy
button. When the model reasons first, a **Thinking** line shows while it works
and becomes **Thought for Ns**; select it to read the reasoning.

When the model line says **accepts images**, you can paste, drop or pick up to
four images per message. Only llama.cpp models with a vision projector accept
images. The browser shrinks each image to at most 1280 pixels on the long edge
and sends it as JPEG. A chat request, images included, must stay under 2 MiB, so
start a new chat if earlier images fill it.

Chat results clear when you leave, change models, or reload. Connected clients
own saved conversations.

## Test transcription

1. Open **Test services → Try a model** and choose a speech-to-text model.
2. Choose an audio file, or select **Record** and speak, then **Stop**.
3. Select **Transcribe** (a recording transcribes when you stop) and review the text.

Any audio the browser can play works. The browser converts it to 16 kHz mono WAV
before upload, up to 8 MiB (about 4 minutes). Recording needs HTTPS or localhost.
Use **auto** or a language code such as `en`.

## Test speech

1. Open **Test services → Speak**.
2. Select a loaded speech model.
3. Enter the text to read and choose an available voice.
4. Generate speech and listen to the result.
5. Save the audio before leaving if needed.

Available voices depend on the backend. A running worker is insufficient to
establish that a particular voice can synthesize intelligible audio.

## Test images

1. Open **Test services → Images**.
2. Choose a loaded image model.
3. Enter a prompt.
4. Use the model's default size, or choose a temporary test size.
5. Generate and inspect the returned image.
6. Save the image before leaving if needed.

Explicit supported request values take precedence over model defaults.
Check [image API limits](../api/automatic1111.md) before configuring another client.

## Compare chat models

1. Load at least two compatible chat models.
2. Open **Test services → Compare**.
3. Select the models and enter a shared prompt. Images are offered when every
   selected model accepts them.
4. Run the comparison and inspect each response and its timing.

Keep prompt, generation settings, and available resources consistent when
comparing. A single test does not establish general model quality or performance.
Use [Diagnostics](diagnostics.md#inspect-requests) to investigate failed requests.
