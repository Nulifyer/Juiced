# Choose a published model variant

Juiced downloads or imports existing weights. It does not convert or quantize
models. A model family can have several repositories, formats and quants. Check
what each candidate contains before you download it.

1. Open **Models → Discover** and search for the model family or publisher.
2. Select a quant chip to open the detail panel. Check the publisher and license.
   Similar model names do not prove a common origin, license or behavior.
3. Compare the quants in the list. Each shows a fit dot, its state on this
   machine and its size. **This machine** spells out the fit for the selected one. Pick one that an installed engine can run, or check that
   engine's requirements in [runtimes and hardware](runtimes.md).
4. A GGUF vision model needs its projector (`mmproj`) file to read images. To add
   a model with its projector, see [import a local file](models.md#import-a-local-file).
5. Check memory separately from storage. The weights, the context and the
   engine's buffers all use memory. For a mixture-of-experts model, the active
   parameter count does not describe how many weights must fit.
6. Choose a **Loading policy** and select **Download**. Juiced adds the quant when the download finishes.
   The new quant is a separate model, so the original stays as it is.
7. Use **Test services** for a quick check, then connect your client through
   **Connect**.

A model's **Overview** tab lists its **Quants** by size. A quant another model
uses shows that model's loading policy and **Open**. A file on disk that no
model uses shows **Not added** and **Add**. A split download that stopped part way shows how many
parts are on disk and **Finish**, which opens it in Discover to fetch the rest. **Get** opens a quant you can still
download in Discover, where you choose the loading policy first. **+N more in
Discover** opens the repository's full list. A model that isn't from Hugging
Face lists only the quants on this machine.

Quant lists read the same in a model's **Overview**, in a file's details and in
Discover: the quant, its state, its size and one action.

Two files in one repository can share a quant label. The Discover list then
shows the part of each file name that differs.

To match settings across quants, use **Other quants** on the model's **Settings**
tab. See [copy settings to other quants](settings.md#copy-settings-to-other-quants).

A higher-bit quant is not automatically the better choice on this host. Compare
quants on the same tasks, engine, context and load. Publisher claims alone do not
establish quality or fit.
