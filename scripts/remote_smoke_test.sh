#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Pipeline · remote smoke test against the DEPLOYED HTTP endpoint.
#
# The role the MCP_* fleet's remote_smoke_test.sh plays: ✗ part of `cargo test`
# or CI, which stay offline. This is the on-demand check that a deployment works
# for a real client — router, TLS, bearer auth, OAuth discovery, the MCP
# handshake, the argument guard, the capability gate — ✗ only that /health
# answers. Run it after every redeploy.
#
# Usage (any cwd · it resolves the repo root itself):
#   scripts/remote_smoke_test.sh                              # DOMAIN + token from .env
#   DOMAIN=https://pipeline.example.com scripts/remote_smoke_test.sh
#   PIPELINE_TOKEN=sk-... DOMAIN=http://127.0.0.1:8080 scripts/remote_smoke_test.sh
#
# Exit 0 only when every check passed. A check that cannot run counts as FAILED.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")/.."

# One value out of .env without executing it. `source` runs every line, and a
# stray line that is not an assignment becomes a command.
env_value() {
  [ -f .env ] || return 0
  sed -n "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*//p" .env | tail -n1 | tr -d '\042\047\r'
}

if [ -z "${DOMAIN:-}" ]; then
  host=$(env_value PIPELINE_DOMAIN)
  DOMAIN="https://${host:?Set DOMAIN, or PIPELINE_DOMAIN in .env}"
fi
DOMAIN="${DOMAIN%/}"
TOKEN="${PIPELINE_TOKEN:-$(env_value PIPELINE_TOKEN)}"
: "${TOKEN:?Set PIPELINE_TOKEN (env var or .env)}"

# The published surface, read from the budget the test suite enforces rather
# than written here a second time where nothing would check it.
budget=$(sed -n 's/^const MAX_TOOLS: usize = \([0-9][0-9]*\);.*/\1/p' crates/pipeline-mcp/tests/surface_budget.rs)
EXPECTED_TOOLS="${EXPECTED_TOOLS:-$budget}"

# The bearer goes in a 0600 header file, ✗ on the command line where `ps` shows it.
auth_header=$(mktemp)
trap 'rm -f "$auth_header"' EXIT
printf 'Authorization: Bearer %s\n' "$TOKEN" >"$auth_header"

CURL=(curl -sS -m 30)
failures=0

check() { # check <label> <command...> · PASS when the command succeeds
  local label=$1
  shift
  if "$@"; then
    echo "  PASS: $label"
  else
    echo "  FAIL: $label"
    failures=$((failures + 1))
  fi
}

# One value from the JSON on stdin (a Python expression over `d`), or nothing
# when the document or the path is absent — the check comparing it then fails.
json_at() {
  python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    v = eval(sys.argv[1], {"__builtins__": {"len": len}}, {"d": d})
except Exception:
    sys.exit(0)
print(v if isinstance(v, str) else json.dumps(v))
' "$1"
}

# POST one JSON-RPC message to /mcp with the bearer · prints the body.
rpc() {
  "${CURL[@]}" -X POST "$DOMAIN/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H "@$auth_header" \
    -d "$1" || true
}

# What a tools/call said: the tool's own text, or the JSON-RPC error message.
said='d["result"]["content"][0]["text"] if "result" in d else d["error"]["message"]'
# Whether it was refused: isError on the result, or a JSON-RPC error.
refused='(d["result"]["isError"] is True) if "result" in d else ("error" in d)'

echo "Target: $DOMAIN"

echo
echo "== public surface =="
health=$("${CURL[@]}" "$DOMAIN/health" || true)
check "/health answers ok" test "$(json_at 'd["ok"]' <<<"$health")" = true
mode=$(json_at 'd["mode"]' <<<"$health")
version=$("${CURL[@]}" "$DOMAIN/version" || true)
version=$(json_at 'd["version"]' <<<"$version")
check "/version names a version (${version:-none}) · mode ${mode:-unknown}" test -n "$version"

echo
echo "== auth + OAuth discovery =="
init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}'
headers=$("${CURL[@]}" -o /dev/null -D - -X POST "$DOMAIN/mcp" \
  -H 'Content-Type: application/json' -d "$init" | tr -d '\r' || true)
code=$(awk '/^HTTP/ {c = $2} END {print c}' <<<"$headers")
check "no token -> 401 (got ${code:-nothing})" test "$code" = 401
hint=$(grep -i '^www-authenticate:' <<<"$headers" | sed -n 's/.*resource_metadata="\([^"]*\)".*/\1/p' | head -n1 || true)
check "the 401 names an absolute metadata URL on $DOMAIN (got '$hint')" test "${hint#"$DOMAIN"/}" != "$hint"
meta=$("${CURL[@]}" "$hint" 2>/dev/null || true)
check "that URL resolves to metadata for $DOMAIN/mcp" test "$(json_at 'd["resource"]' <<<"$meta")" = "$DOMAIN/mcp"
for doc in oauth-protected-resource oauth-authorization-server; do
  code=$("${CURL[@]}" -o /dev/null -w '%{http_code}' "$DOMAIN/.well-known/$doc/mcp" || true)
  check "path-inserted /.well-known/$doc/mcp -> 200 (got ${code:-nothing})" test "$code" = 200
done
as=$("${CURL[@]}" "$DOMAIN/.well-known/oauth-authorization-server" || true)
check "authorization server issuer is $DOMAIN" test "$(json_at 'd["issuer"]' <<<"$as")" = "$DOMAIN"
check "PKCE offers S256 only" test "$(json_at 'd["code_challenge_methods_supported"]' <<<"$as")" = '["S256"]'
who=$("${CURL[@]}" -H "@$auth_header" "$DOMAIN/tokens/whoami" || true)
check "the token authenticates (principal: $(json_at 'd.get("token")' <<<"$who"))" \
  test "$(json_at 'd["authenticated"]' <<<"$who")" = true

echo
echo "== MCP =="
r=$(rpc "$init")
check "initialize -> serverInfo pipeline-mcp" \
  test "$(json_at 'd["result"]["serverInfo"]["name"]' <<<"$r")" = pipeline-mcp
r=$(rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
n=$(json_at 'len(d["result"]["tools"])' <<<"$r")
check "tools/list publishes $EXPECTED_TOOLS tools (got ${n:-none})" test "$n" = "$EXPECTED_TOOLS"
# The Anthropic API refuses allOf · anyOf · oneOf at the top of a tool's input
# schema, and a Claude client drops such a tool without failing the connection.
# Every other check in this file passed while all 19 tools were dropped.
dropped=$(json_at '[t["name"] for t in d["result"]["tools"] if "allOf" in t["inputSchema"] or "anyOf" in t["inputSchema"] or "oneOf" in t["inputSchema"]]' <<<"$r")
check "no tool schema a Claude client would drop (top-level combinator in: ${dropped:-unreadable})" \
  test "$dropped" = "[]"
r=$(rpc '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"version"}}}')
check "pipeline_meta.version succeeds" test "$(json_at "$refused" <<<"$r")" = false

# An argument no tool declares must be an error, ✗ silently dropped. The probe
# name does not contain the phrase asserted, so an echo of it cannot pass.
r=$(rpc '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"version","smoke_undeclared_arg":1}}}')
check "an undeclared argument is refused" test "$(json_at "$refused" <<<"$r")" = true
check "the refusal names the argument" \
  grep -qF "unknown argument 'smoke_undeclared_arg'" <<<"$(json_at "$said" <<<"$r")"
# The same inside `args`, where the per-action validator answers.
r=$(rpc '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"version","args":{"smoke_undeclared_arg":1}}}}')
check "an undeclared argument inside args is refused by name" \
  grep -qF "unknown argument 'smoke_undeclared_arg'" <<<"$(json_at "$said" <<<"$r")"
# A misspelled action is an unknown action that names the real ones, ✗ a
# permission error. The probe is `verzion`, so echoing it cannot match `version`.
r=$(rpc '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"verzion"}}}')
check "a misspelled action is refused as unknown, naming 'version'" \
  grep -qE "unknown action.*known:.*version" <<<"$(json_at "$said" <<<"$r")"

case "$mode" in
  read_only)
    r=$(rpc '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"pipeline_docker","arguments":{"action":"build"}}}')
    check "read_only blocks a destructive action (pipeline_docker.build)" \
      grep -qF 'blocked by PIPELINE_REMOTE_MODE=read_only' <<<"$(json_at "$said" <<<"$r")"
    # Runs `cargo test` · a "read" that executes the project's code stays blocked.
    r=$(rpc '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"pipeline_test","arguments":{"action":"flake_detect"}}}')
    check "read_only blocks a read that executes code (pipeline_test.flake_detect)" \
      grep -qF 'blocked by PIPELINE_REMOTE_MODE=read_only' <<<"$(json_at "$said" <<<"$r")"
    # A pure read the gate used to refuse as "destructive" reaches its handler.
    # Asserted on the answer existing AND carrying no gate text, so an empty or
    # missing reply cannot pass.
    r=$(rpc '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"health"}}}')
    reached() { [ -n "$1" ] && ! grep -qF 'blocked by' <<<"$1"; }
    check "read_only lets a pure read through (pipeline_meta.health)" \
      reached "$(json_at "$said" <<<"$r")"
    ;;
  full)
    # ✗ probe the gate in full mode — the probe would really build an image.
    echo "  SKIP: capability gate · mode is full, a destructive probe would execute"
    ;;
  *)
    check "/health reports a known mode (got '$mode')" false
    ;;
esac

echo
if [ "$failures" -eq 0 ]; then
  echo "All checks passed against $DOMAIN."
else
  echo "$failures check(s) FAILED against $DOMAIN."
  exit 1
fi
