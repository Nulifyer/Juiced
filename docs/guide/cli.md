# Inspect Juiced from a terminal

The container includes a native command-line client named `lemonade`.
Use an explicit port because the container serves HTTP on `8080` and disables
beacon broadcast by default.

## Check a running container

```bash
docker exec juiced lemonade --no-discovery --port 8080 status
docker exec juiced lemonade --no-discovery --port 8080 list --downloaded
docker exec juiced lemonade --no-discovery --port 8080 backends
```

These commands inspect the existing server. Use `docker logs --tail 100 juiced`
for container output. The native `logs` command opens a web view and is less
useful in a headless container.

If authentication is configured, the CLI requires an appropriate credential.
Use the operator credential for management commands. `LEMONADE_API_KEY` supplies
the CLI credential through its environment. See [access and API keys](access.md).

## Use a separately built client

Point a native client at the actual Juiced host and port:

```bash
lemonade --no-discovery --host 127.0.0.1 --port 8080 status
```

Replace the host for a remote server. The native client defaults to port `13305`,
so omitting `--port 8080` targets a different address.

## Find command help

```bash
docker exec juiced lemonade --help
docker exec juiced lemonade list --help
```

The executable retains inherited commands and help text. Commands that open the
upstream desktop/web application or launch an external agent are separate from
Juiced's current console workflows. Use the browser for [model acquisition](models.md),
[loading policies](loading-policy.md), and [runtime selection](runtimes.md).

For scripts using Juiced-specific controls, follow the [native API guide](../api/juiced.md).
