# Putting Pipeline behind a reverse proxy

Pipeline's HTTP transport listens on plain HTTP (`:8080` in the container) and never
binds `:80`/`:443` itself. Exposing it publicly means putting a TLS-terminating reverse
proxy of your choice in front of it. Any proxy works when it meets the contract below.

## Routes

| Path | Upstream | Auth |
|---|---|---|
| `/mcp` | `pipeline:8080` · MCP JSON-RPC | Bearer |
| `/tokens/whoami` | `pipeline:8080` · token sanity check | Bearer |
| `/.well-known/oauth-*` | `pipeline:8080` · OAuth discovery | public |
| `/oauth/*` | `pipeline:8080` · authorize · token · register | public |
| `/health` | `pipeline:8080` · liveness | public |
| `/version` | `pipeline:8080` · running version | public |

Pipeline is an MCP endpoint, not a web app: there is no browser UI. The proxy terminates
TLS and forwards `Authorization: Bearer` untouched; the app is the sole auth gate.
`/health` and `/version` bypass auth so a monitor can alert on a dead or stale deployment
without holding a token; neither echoes a token value.

## What the proxy must do

- **No response buffering on `/mcp`.** MCP's Streamable HTTP can answer with SSE, and a
  buffering proxy deadlocks a long tool call: the server returns 200 and the client hangs.
- **Forward `Host`, `X-Forwarded-Proto` and `X-Forwarded-Host`.** Behind the proxy the socket
  is plain HTTP; without these Pipeline advertises its internal address in the OAuth
  metadata and sends the user's browser to an unreachable host.
- **Write the client's address as the last `X-Forwarded-For` hop** (set or append). Pipeline
  keys its rate limit on the last hop, never the first: a client can forge the header, and a
  forged value lands to the left of the one the proxy wrote.
- **Generous timeouts** (300 s read and write): builds, RE jobs and long tools run that long.
- **Security headers:** `Strict-Transport-Security`, `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and CORS for the connector's origin
  (`Access-Control-Allow-Origin: https://claude.ai`, methods `GET, POST, OPTIONS`, headers
  `Authorization, Content-Type, MCP-Session-Id`). GET is required: OAuth discovery and the
  authorize form are GETs.

## Reaching the container

`docker-compose.yml` exposes no host port. It creates the `pipeline_net` bridge network;
attach your proxy to it and point it at `pipeline:8080`, or publish the port to
`127.0.0.1` in a local override and proxy to that.
