# Configure access and API keys

Choose who can reach the operator console and who can call inference APIs.
The local installation example publishes HTTP only on `127.0.0.1:8080`.

## Allow remote access

1. Choose a private interface, VPN, or authenticated HTTPS reverse proxy.
2. Configure it to reach the container's HTTP port `8080`.
3. Open the configured address from the intended client machine.
4. Test a model request through that address.

A remote client's `localhost` means the client machine. Use the Juiced host's
reachable address instead. The UI and API share the listener. An anonymous
installation exposes management operations to any caller that can reach it.

## Configure HTTP API keys

The current native server retains these environment variable names:

| Variable | Purpose |
| --- | --- |
| `LEMONADE_API_KEY` | Regular API access, including inference |
| `LEMONADE_ADMIN_API_KEY` | Operator access, including configuration and model lifecycle controls |

Supply different values through your deployment's secret handling. Pass them to
the container, for example with Compose environment entries:

```yaml
    environment:
      LEMONADE_API_KEY: "${LEMONADE_API_KEY:?Set the inference key}"
      LEMONADE_ADMIN_API_KEY: "${LEMONADE_ADMIN_API_KEY:?Set the operator key}"
```

Recreate the container after changing environment variables. Preserve `/data`.
Check an authenticated discovery request:

```bash
curl --fail-with-body http://localhost:8080/v1/models \
  -H "Authorization: Bearer $LEMONADE_API_KEY"
```

Use the operator key for native control operations. If both configured keys have
the same value, that credential has full operator access. A dummy key in an
anonymous deployment does not create a private user.

The current console sends same-origin requests without an API-key entry form.
For an authenticated console, an access-controlled proxy must supply the operator
credential to its upstream requests. Keep that proxy entry restricted to trusted
operators. An inference client's key should not gain access through this operator
entry point.

## Keep Wyoming private

The optional Wyoming speech listener uses TCP port `10300` and has no protocol
authentication. Keep it on a private network reachable by Home Assistant.
HTTP API keys and HTTPS routing do not secure that separate TCP listener.
See [Home Assistant speech](../integrations/home-assistant.md).

## Understand retained state

Prompt snapshots can contain sensitive computation state. Restrict access to
the persistent volume and its backups. Session IDs partition cache reuse, and
do not establish user authority. See [cache](cache.md) for identity boundaries.
