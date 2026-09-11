---
title: Connecting an editor (ACP)
description: Point Zed or another ACP client at jan-klod, and what to check by hand.
---

# Connecting an editor (ACP)

`jan-klod-gateway acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com)
agent side on stdio, so an ACP editor drives a turn without a bespoke plugin.

```sh
cd ~/my-project
jan-klod-gateway acp
```

The editor spawns it, owns the process, and talks over its stdin and stdout —
no port and no token, exactly like `jan-klod-gateway rpc`. Run it from the
repository you want it to work on: the gateway uses the current directory as
the workspace, so file tools are jailed to it.

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

Zed's own configuration is the authority on that key's shape; check its docs if
this does not take, since editor settings move faster than this page.

## What an editor can do that an MCP client cannot

**Answer a confirmation.** A turn that wants to write a file or run a command
stops and asks, and over ACP that question reaches the editor as
`session/request_permission` with the same options the permission gate offers —
so you can allow it. Over the [MCP port](../quickstart.md) nobody can answer, so
every confirmation takes its denying default and a turn there never writes.

Everything that cannot be read as an answer refuses: a cancelled outcome, a
selection naming no option, an empty result, a protocol error, a closed pipe.
An answer this cannot understand never widens a permission.

## The editor's project directory is not the workspace

`session/new` carries the editor's `cwd`, and jan-klod **reports it back
without adopting it**. The workspace is a grant — `workspace:` in `config.yaml`,
or the directory the gateway was started in — and letting a client retarget it
would let the editor choose what the file tools may reach.

So if the editor's project and the gateway's workspace differ, the file tools
follow the *gateway*. Start it in the directory you mean.

## Verifying by hand

**This is the one part no test covers.** The offline fixtures in
`host/tests/it/acp.rs` drive the protocol directly: the handshake, a prompt with
streamed updates, a permission granted and refused, a cancel. What they cannot
show is that a real editor's idea of ACP matches ours — so a first connection
with Zed is a manual check, and it is not verified by CI.

What to look for, in order:

1. **The handshake.** The editor should list jan-klod as an available agent. If
   it does not, the failure is before ACP: check the binary is on `PATH` and
   that `jan-klod-gateway acp` starts without printing to stdout. Stdout carries
   protocol frames and nothing else; anything else there breaks the stream, and
   logs go to stderr for that reason.
2. **A read-only turn.** Ask something that needs no permission ("what does this
   repo do?"). The answer should stream rather than arriving in one piece.
3. **A write.** Ask for an edit. A confirmation should appear *in the editor*;
   allowing it should produce the file, and refusing it should not.
4. **A cancel.** Start a long turn and stop it. The editor should not hang: the
   turn is answered with `cancelled` rather than left open.

If step 3 shows no prompt at all, the permission gate is probably disabled in
`config.yaml` (`interceptor.permission.enabled`) — in which case the write
happens unasked, which is the gate's absence rather than ACP's.
