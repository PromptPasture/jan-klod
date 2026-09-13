---
title: Connecting an editor (ACP)
description: Point Zed or another ACP client at jan-klod, and what to check by hand.
---

# Connecting an editor (ACP)

`jan-klod-gateway acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com)
on stdio, letting an ACP editor drive turns without a plugin.

```sh
cd ~/my-project
jan-klod-gateway acp
```

The editor spawns it, owns the process, and talks over stdin/stdout — no port,
no token. Run it from the directory you want as the workspace; file tools are jailed to it.

**In Zed**, add it as an external agent server in `settings.json`:

```json
{
  "agent_servers": {
    "jan-klod": {
      "command": "jan-klod-gateway",
      "args": ["acp"]
    }
  }
}
```

Check Zed's docs if this does not work; editor settings may have changed.

## What an editor can do that an MCP client cannot

**Answer a confirmation.** Writes and commands trigger `session/request_permission`
in the editor, with the same options as the permission gate — you can allow or
deny. Over [MCP](../quickstart.md), confirmations cannot be answered, so all
default to denial and nothing writes.

Any unrecognized answer (cancel, empty, protocol error) refuses. An answer never
widens permissions beyond what was requested.

## Workspace vs. editor project

The workspace is set at gateway startup — either `workspace:` in `config.yaml`,
or the directory the gateway was started in — and is not overridden by the
editor's project directory. If they differ, file tools follow the gateway's
workspace, so file operations are scoped to where the gateway started. Start
the gateway in the directory you mean.

## Verifying by hand

The offline fixtures in `host/tests/it/acp.rs` test the protocol directly
(handshake, streamed turns, permissions, cancels), but a real editor's ACP
implementation requires manual verification on first connection.

What to check:

1. **Handshake.** Jan-klod appears as an available agent. If not, the failure is before ACP:
   verify the binary is on `PATH` and `jan-klod-gateway acp` starts silently.
   Stdout must carry only protocol frames; anything else breaks the stream.
2. **Read-only turn.** Ask something that needs no permission. The answer should stream
   rather than arriving in one piece.
3. **Write.** Request an edit. A confirmation should appear in the editor;
   allowing it should create the file, refusing should not.
4. **Cancel.** Start a long turn and stop it. The editor should not hang; the
   turn is answered with `cancelled`.

If step 3 shows no prompt at all, the permission gate is probably disabled in
`config.yaml` (`interceptor.permission.enabled`) — in which case the write
happens unasked, which is the gate's absence rather than ACP's.
