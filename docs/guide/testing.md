# Test model services

Use **Test services** to check a loaded model before connecting another app.
Enable the model and wait for its worker to load first.

## Test chat

1. Open **Test services → Chat**.
2. Choose a loaded model.
3. Send a short prompt.
4. Check that the answer completes.
5. Use **Stop generation** if you need to cancel the request.

Attach an image only when the selected model has working vision support and its
required components. Recording from the microphone requires browser permission
and a secure browser context, normally HTTPS or localhost.

Chat results clear when you leave, change models, or reload. Connected clients
own saved conversations.

## Test transcription

1. Open **Test services → Transcribe**.
2. Select a loaded transcription model.
3. Drop or choose an audio file up to 120 seconds long.
4. Start transcription and review the text.
5. Copy or download the transcript if you need to keep it.

The browser converts audio to 16 kHz mono PCM. This test requests English
transcription. The 120-second limit belongs to this browser test flow.

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
3. Select the models and enter a shared prompt.
4. Run the comparison and inspect each response and its timing.

Keep prompt, generation settings, and available resources consistent when
comparing. A single test does not establish general model quality or performance.
Use [Diagnostics](troubleshooting.md) to investigate failed requests.
