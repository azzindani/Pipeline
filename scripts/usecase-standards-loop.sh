#!/usr/bin/env bash
# Live MCP use case · the standards-guided development loop.
#
# Drives Pipeline's REAL MCP server (stdio transport) through the sequence a cold-started
# agent walks when it picks up work on a standards-governed project:
#
#   initialize                      handshake — prove the endpoint answers
#   pipeline_meta.version           what am I talking to
#   pipeline_session.handover       where things stand — Pipeline owns the context
#   pipeline_standards.route        WHY these standards bind this project (provenance)
#   pipeline_standards.brief        ★ THE INJECTION — the L0 "Standards in force" packet
#   pipeline_standards.show rust    L1 drill-down — one binding standard, in full
#   pipeline_standards.checklist    L2 — the concrete obligations (enforcement surface)
#   pipeline_standards.check        the compliance gate — surfaces drift, recommends a fix
#
# Everything here is read-only and offline: standards resolve from the local clone named by
# `standards.source` in pipeline.yaml (no network). Run from anywhere — the script cd's to
# the repo root, because the standards handler resolves pipeline.yaml from the cwd.
#
# Usage:  scripts/usecase-standards-loop.sh            # human-readable, sectioned
#         scripts/usecase-standards-loop.sh --raw      # raw JSON-RPC responses, one per line
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="$ROOT/target/debug/pipeline"
if [[ ! -x "$BIN" ]]; then
	echo "building pipeline (debug) — first run only…" >&2
	cargo build -p pipeline-cli >&2
fi

# The call sequence, one JSON-RPC envelope per line. tools/call carries the tool in
# params.name and the action + nested args in params.arguments.{action,args} — the exact
# envelope both the stdio and HTTP transports read.
REQUESTS=$(cat <<'JSONL'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"usecase","version":"1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"version"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pipeline_session","arguments":{"action":"handover"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"pipeline_standards","arguments":{"action":"route"}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"pipeline_standards","arguments":{"action":"brief"}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"pipeline_standards","arguments":{"action":"show","args":{"id":"rust"}}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"pipeline_standards","arguments":{"action":"checklist"}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"pipeline_standards","arguments":{"action":"check"}}}
JSONL
)

RESP_FILE="$(mktemp)"
trap 'rm -f "$RESP_FILE"' EXIT
printf '%s\n' "$REQUESTS" | "$BIN" mcp --transport stdio 2>/dev/null >"$RESP_FILE"

if [[ "${1:-}" == "--raw" ]]; then
	cat "$RESP_FILE"
	exit 0
fi

# Human-readable projection. Kept deliberately small: each tool call is unwrapped to the
# handful of fields that carry the point, not the whole payload — the raw view is one flag away.
#
# ! Responses reach python via a FILE (RESP_FILE), never stdin: `python3 <<'PY'` already
# uses stdin to read the program itself, so piping data in too would collide — python would
# see an empty stdin and SIGPIPE the writer.
RESP_FILE="$RESP_FILE" python3 <<'PY'
import json, os

def load(line):
    try: return json.loads(line)
    except json.JSONDecodeError: return None

def unwrap(msg):
    """tools/call result → the tool's own ToolResponse (ok/data/error), or None."""
    try:
        return json.loads(msg["result"]["content"][0]["text"])
    except (KeyError, IndexError, TypeError, json.JSONDecodeError):
        return None

def rule(title):
    print("\n" + "═" * 78)
    print(title)
    print("═" * 78)

with open(os.environ["RESP_FILE"], encoding="utf-8") as fh:
    responses = fh.read().splitlines()

for line in responses:
    line = line.strip()
    if not line:
        continue
    msg = load(line)
    if msg is None:
        continue
    mid = msg.get("id")

    if mid == 1:
        rule("1 · initialize — the endpoint answers")
        si = msg["result"]["serverInfo"]
        print(f"server   {si['name']} v{si['version']}")
        print(f"protocol {msg['result']['protocolVersion']}")
        continue

    tr = unwrap(msg)
    if tr is None:
        rule(f"id {mid} — unexpected shape")
        print(json.dumps(msg)[:400])
        continue
    d = tr.get("data", {})

    if mid == 2:
        rule("2 · pipeline_meta.version — what am I talking to")
        print(json.dumps(d, indent=2)[:600])

    elif mid == 3:
        rule("3 · pipeline_session.handover — Pipeline reports REAL state, never invents it")
        if tr.get("ok"):
            s = json.dumps(d, indent=2)
            print(s[:1200] + ("\n… (truncated)" if len(s) > 1200 else ""))
        else:
            # Honest empty state: a cold clone has recorded no session, so there is no
            # handover packet to hand over. Pipeline says so rather than fabricating one.
            print(f"ok       {tr.get('ok')}")
            print(f"error    {tr.get('error')}")
            print("→ no project/session recorded yet — the standards surface below needs")
            print("  neither; it reads pipeline.yaml + the corpus, so it is the cold-start entry.")

    elif mid == 4:
        rule("4 · pipeline_standards.route — WHY these standards bind")
        print(f"runtime       {d.get('runtime')}")
        print(f"project_type  {d.get('project_type')}")
        print(f"surfaces      {d.get('surfaces')}")
        print(f"sha           {d.get('sha')}")
        routed = d.get("routed", {})
        ids = routed.get("ids", [])
        print(f"bound ({len(ids)}): {', '.join(ids)}")
        if routed.get("decisions"):
            print(f"decisions     {routed['decisions']}")
        if routed.get("unknown_routes"):
            print(f"unknown       {routed['unknown_routes']}")

    elif mid == 5:
        rule("5 · pipeline_standards.brief — ★ THE INJECTION (L0 context packet)")
        print(d.get("markdown", "")[:2400])

    elif mid == 6:
        rule("6 · pipeline_standards.show rust — L1, one binding standard in full")
        print(f"id       {d.get('id')}   tier {d.get('tier')}   v{d.get('version')}")
        print(f"owns     {str(d.get('owns'))[:200]}")
        content = d.get("content", "")
        print(f"content  {len(content)} chars — first 900:")
        print(content[:900])

    elif mid == 7:
        rule("7 · pipeline_standards.checklist — L2, the enforcement surface")
        print(f"standards    {d.get('standards')}")
        print(f"total_items  {d.get('total_items')}")
        for c in (d.get("checklists") or [])[:2]:
            print(f"\n  [{c.get('id')}] {len(c.get('items', []))} items — first 3:")
            for it in c.get("items", [])[:3]:
                print(f"    · {str(it)[:110]}")

    elif mid == 8:
        rule("8 · pipeline_standards.check — the compliance gate")
        print(f"ok (compliant)   {tr.get('ok')}")
        print(f"bound_standards  {d.get('bound_standards')}")
        print(f"obligations      {d.get('obligations')}")
        blocking = d.get("blocking", [])
        print(f"blocking ({len(blocking)}):")
        for b in blocking:
            print(f"    ✗ {b}")
        print(f"next_suggested   {tr.get('next_suggested')}")

print("\n" + "═" * 78)
print("End of loop. The agent now holds the ruling standards as context and the one")
print("blocking finding the gate surfaced — ready to adjudicate. See the doc for the rest.")
print("═" * 78)
PY
