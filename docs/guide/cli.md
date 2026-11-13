# Manage Juiced from a terminal

The container includes a command-line client named `lemonade`. Inside the
container it talks to the service on `127.0.0.1:8080` without extra flags.

```bash
docker exec juiced lemonade status
docker exec juiced lemonade list --downloaded
```

Outside the container, name the server explicitly:

```bash
lemonade --host 192.168.1.10 --port 8080 status
```

Without a target, the client tries `127.0.0.1:13306` and then local discovery.

## Credentials

If API keys are configured, management commands need the operator key. The
client reads `LEMONADE_ADMIN_API_KEY`, then `LEMONADE_API_KEY`; `--api-key KEY`
overrides both. A key is sent only to an explicit target (`--host`/`--port`,
`--url`, or `LEMONADE_HOST`/`LEMONADE_PORT`, which the container sets). See
[access and API keys](access.md).

## Install an engine

Models need an installed engine build. Pick one from the list:

```bash
lemonade backends install llamacpp:cpu
lemonade backends uninstall llamacpp:cpu
```

`backends uninstall` refuses while a registered model still uses that build.
Delete the model first. For the full engine list and its state, open
**Engines & hardware** in the console or call `GET /v1/system-info`.

## Download and register a model

```bash
# A catalog model, by its native name
lemonade pull Qwen3-0.6B-GGUF

# Any Hugging Face GGUF repository; the quantization is optional
lemonade pull MyModel --checkpoint unsloth/Qwen3-8B-GGUF:Q4_K_M
```

`pull` prints progress, verifies every file and registers the model as
**On demand**. A `--checkpoint` pull is registered as `user.MyModel`; clients
call it `MyModel`. `--recipe` defaults to `llamacpp`.

A download that is interrupted keeps running on the server. Check it with
`GET /v1/downloads`.

Not supported: ModelScope, collections, and a separate projector (`mmproj`)
file. Vision models work when the catalog bundles their projector.

## Load, unload and delete

```bash
lemonade load MyModel
lemonade load MyModel --ctx-size 8192 --llamacpp-args "-np 2"
lemonade unload MyModel
lemonade unload             # every loaded model
lemonade delete MyModel     # unload, remove, free its downloaded files
```

Load options are saved for the model and apply on every later load.
`--llamacpp-args` accepts the typed llama.cpp flags (`--ctx-size`, `--threads`,
`--gpu-layers`, `--parallel`, batch sizes, `--flash-attn`, `--cache-type-k/-v`
and the sampling defaults) and saves them as the model's settings. Other flags
belong in the model's extra arguments. `--pin` sets the model to **Hot**.

`delete` frees only files Juiced downloaded and no other model uses. Files you
linked from your own folders stay on disk. `delete MODEL --revision N` removes
the registration only and keeps every file.

## Logs and other commands

```bash
lemonade logs --limit 100
lemonade logs --source MyModel
lemonade --help
```

`logs` prints the most recent service and worker lines. `docker logs juiced`
shows the same service output.

The client never starts or stops the service. For settings the CLI does not
cover, use the console or the [API](../api/juiced.md).

## The v0.9.2 client

The `lemonade` binary from Juiced v0.9.2 also works against this service for
`list`, `pull`, `load`, `unload`, `delete`, `pin`/`unpin`, `alias` and
`backends install|uninstall`. It defaults to port `13305`, so pass
`--port 8080`.
