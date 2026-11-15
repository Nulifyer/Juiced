# Test model services

Use **Test services** to check a model before you connect another app. The model
must be Hot or On demand. A model that is not loaded loads on first use.

**Try a model** has one model menu, grouped by service. The page shows the form
for the chosen model's service.

## Test chat

1. Open **Test services → Try a model**.
2. Choose a chat model.
3. Send a short prompt and check that the answer completes.
4. Use **Stop** to cancel the request. **New chat** clears the conversation.

**Max reply** caps each reply in tokens, up to the model's output limit. Turn
**Thinking** off to ask a reasoning model to answer directly. Other models
ignore it.

Replies render as Markdown: tables, lists, links and code blocks with a copy
button. When the model reasons first, a **Thinking** line shows while it works
and becomes **Thought for Ns**. Select it to read the reasoning.

When the model line says **accepts images**, you can paste, drop or pick up to
four images per message. Only llama.cpp models with a vision projector accept
images. The browser shrinks each image to at most 1280 pixels on the long edge
and sends it as JPEG. A chat request, images included, must stay under 2 MiB, so
start a new chat if earlier images fill it.

Chat results clear when you leave, change models, or reload. Connected clients
own saved conversations.

## Test transcription

1. Choose a speech-to-text model under **Try a model**.
2. Choose an audio file, or select **Record** and speak, then **Stop**.
3. Select **Transcribe** and review the **Transcript**. A recording transcribes
   when you stop.

Any audio the browser can play works. The browser converts it to 16 kHz mono WAV
before upload, up to 8 MiB (about 4 minutes). Recording needs HTTPS or localhost.
Enter **auto** or a language code such as `en` under **Language**.

## Test speech

1. Choose a text-to-speech model under **Try a model**.
2. Enter the **Text** to read.
3. Choose a **Voice**, a speed and an audio format.
4. Select **Speak** and listen to the result.
5. Select **Download** to keep the audio before you leave.

Available voices depend on the engine. A running worker does not prove that a
voice produces intelligible audio.

## Test images

1. Choose an image model under **Try a model**.
2. Enter a **Prompt**, and optionally what to **Avoid**.
3. Choose a **Size**. Leave **Steps** and **Seed** empty to use the model's defaults.
4. Select **Generate** and inspect the image.
5. Select **Download** to keep the image before you leave.

Values you set here override the model's image defaults for this request.
Check [image API limits](../api/automatic1111.md) before configuring another client.

## Test embeddings

1. Choose an embedding model under **Try a model**.
2. Enter a few texts, one per line.
3. Select **Embed** and compare the cosine similarity of each pair.

## Test audio and 3D generation

Audio models take a prompt, optional lyrics, a length and a seed, then select
**Generate**. 3D models take an object photo, then select **Generate mesh** and
download the `.glb` file. Both can take minutes, and the first request also loads
the model.

## Compare chat models

1. Open **Test services → Compare**. It needs at least two chat models that are
   Hot or On demand.
2. Choose two models, and an optional third.
3. Enter a shared prompt and select **Compare**. Images are offered when every
   selected model accepts them.
4. Read each reply with its **First token**, **Total** and **Speed** figures.

Keep the prompt, settings and host load the same when you compare. A single test
does not establish general model quality or performance.
Use [Diagnostics](diagnostics.md#inspect-requests) to investigate failed requests.
