# jan-klod-host

The `jan-klod-gateway` binary and the surfaces it serves: REST and SSE, WebSocket, ACP, MCP, and Telegram. Each surface is a transport over a kernel `AgentSession`; none of it is agent behaviour.

Depends on [`jan-klod-core`](../core), [`jan-klod-config`](../config), [`jan-klod-protocol`](../protocol), [`jk-session`](../session), and embeds the web client from [`pkgs/clients/web/dist`](../../clients/web).

```sh
make serve                  # run the gateway against config.yaml and the staged ext/
make test                   # host and guest unit tests
make gate                   # the integration suite (tests/it), which needs the guests staged
```

Setup and first run: [quickstart](../../../docs/quickstart.md).
