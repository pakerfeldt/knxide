# knxide-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server for a KNX bus. It lets an
AI assistant (Claude Code, Claude Desktop, or any MCP client) find your group addresses by
name, read their values, follow bus traffic and, if you allow it, switch things.

```sh
cargo install --path apps/knxide-mcp
claude mcp add knx -- knxide-mcp --gateway 192.168.1.50 --project /path/to/knx.xml
```

Then ask things like "what's the temperature in the living room?" or "which lights changed
in the last minute?".

## Tools

| Tool | What it does |
|---|---|
| `bus_status` | Gateway, connection, project and whether writing is on |
| `find_group_addresses` | Search the ETS project by name, room, address or DPT. Shows each address's DPT, the value format it accepts, and the last value seen |
| `read_group_address` | Send a `GroupValue_Read` and decode the answer |
| `recent_telegrams` | Group telegrams seen since the server connected (the last 500) |
| `write_group_address` | Send a `GroupValue_Write`. Only with `--allow-write` |

Addresses can be given as `1/2/3` or by their exact name in the project. Values are text in
the same format as the `knxide` CLI: `on`, `21.5`, `comfort`, `#ff8800`, and so on.

## Options

- `--gateway IP[:PORT]`: the KNX IP interface or router to tunnel to. Defaults to
  `$KNXIDE_GATEWAY`.
- `--tcp`: tunnel over TCP.
- `--routing` (or `KNXIDE_ROUTING=1`): use KNXnet/IP routing (multicast `224.0.23.12`)
  instead of a tunnel. Needs a KNX IP router on the network, but takes none of its tunnel
  slots, which helps when other software already holds them. Overrides `--gateway`.
  - `--interface IP` (or `$KNXIDE_INTERFACE`): the local interface for multicast. Defaults
    to the one the OS routes `224.0.23.12` through; set it on hosts with several networks
    (Docker, VPNs).
  - `--address IA` (or `$KNXIDE_ADDRESS`): the individual address our telegrams carry.
    Default `15.15.250`.
- `--project FILE`: ETS group address export (CSV or XML). Without it the model only sees
  bare addresses and cannot decode values. Defaults to `$KNXIDE_PROJECT`, then `./knx.xml`.
  The MCP client decides the working directory, so give an absolute path. DPT overrides
  saved by knxide-monitor (`FILE` with `.overrides.json`) are applied.
- `--allow-write`: offer `write_group_address`. Off by default, so the model can look but
  not touch.
- `--lazy`: connect on the first tool call instead of at start-up. `recent_telegrams` only
  has traffic from after the connection opened.

The server holds one tunnel for as long as the client keeps it running. Interfaces have only
a few tunnels; if they are all in use, use `--routing` (with a KNX IP router) or point it at
[knxide-proxy](../knxide-proxy).

Logs go to stderr (`RUST_LOG=knxide_mcp=debug` for more). Every tool call is logged.

## Running it as a service (HTTP)

With `--listen IP:PORT` (or `$KNXIDE_MCP_LISTEN`) it serves MCP's Streamable HTTP transport at
`http://IP:PORT/mcp` instead of stdio, and keeps running. All clients share one tunnel, and
`recent_telegrams` keeps its history across them.

Set `KNXIDE_MCP_TOKEN`, and clients must send `Authorization: Bearer <token>` (401
otherwise). Without a token it refuses to listen on anything but a loopback address. Without
a token it also refuses browser requests (anything with an `Origin` header). It stops cleanly
on SIGTERM and closes the tunnel, so `docker stop` frees it on the interface.

```sh
KNXIDE_MCP_TOKEN=$(openssl rand -hex 32) knxide-mcp --listen 0.0.0.0:8092 \
    --gateway 192.168.1.50 --project /path/to/knx.xml --allow-write
```

Client side, e.g. a project's `.mcp.json` for Claude Code:

```json
{
  "mcpServers": {
    "knx": {
      "type": "http",
      "url": "http://homeserver:8092/mcp",
      "headers": { "Authorization": "Bearer ${KNX_MCP_TOKEN}" }
    }
  }
}
```

Only plain JSON responses: `GET` and `DELETE` on `/mcp` answer 405 (no event stream, no
sessions).

### Docker

`apps/knxide-mcp/Dockerfile` builds from the repository root, on x86_64 or aarch64. The image
listens on `0.0.0.0:8092` and reads the project from `/data/knx.xml`, so mount it there (and
`knx.overrides.json` next to it, if you have one):

```sh
docker build -f apps/knxide-mcp/Dockerfile -t knxide-mcp .
docker run -d --name knxide-mcp -p 8092:8092 \
    -e KNXIDE_GATEWAY=192.168.1.50 -e KNXIDE_MCP_TOKEN=... \
    -v /path/to/knx.xml:/data/knx.xml:ro knxide-mcp --allow-write
```

Tunnelling uses route-back (NAT) mode, so the default bridge network works. Routing needs
`--network host` (`network_mode: host` in compose), since multicast does not cross Docker's
bridge; set `KNXIDE_INTERFACE` to the host's LAN address:

```sh
docker run -d --name knxide-mcp --network host \
    -e KNXIDE_ROUTING=1 -e KNXIDE_INTERFACE=192.168.1.20 -e KNXIDE_MCP_TOKEN=... \
    -v /path/to/knx.xml:/data/knx.xml:ro knxide-mcp --allow-write
```

## Protocol

MCP over stdio (newline-delimited JSON-RPC 2.0) or Streamable HTTP. Implements `initialize`,
`ping`, `tools/list` and `tools/call`; protocol versions 2024-11-05 to 2025-11-25.
