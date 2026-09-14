# Auth audit — `pipeline-mcp` against `security/TOKENS.md`

> What Pipeline's HTTP auth surface does today, checked row by row against the token standard it is the first consumer of.

Audited: `crates/pipeline-mcp/src/auth.rs` (227 LOC) · `oauth.rs` (812) · `ratelimit.rs` (308).
Standard: [security/TOKENS.md](https://github.com/azzindani/Standards/blob/main/security/TOKENS.md).

---

## 1. Verdict

Structurally sound. Token format choice is correct for the surface, the
comparison is constant-time, and the server refuses to start unauthenticated —
a stricter default than the implementation it was ported from.

Three findings, one of which is a real weakening of a rule the standard calls
load-bearing.

| # | Finding | Severity |
|---|---|---|
| 1 | Access-token TTL is 24 h against a ≤ 15 min ceiling for user-facing tokens | High · **fixed** |
| 2 | Refresh-token reuse is rejected but the chain is not revoked | Medium · **fixed** |
| 3 | No redaction rule at the logging boundary — relies on call sites | Low · open |

---

## 2. What holds

| Standard rule | Status |
|---|---|
| Format follows the revocation requirement (§1) | ✓ Opaque random tokens · ✗ JWT · revocation is a store lookup, the strongest option |
| One format per interface (§1) | ✓ Bearer only · static registry and OAuth grants share one validation path |
| ✗ `alg: none`, no algorithm confusion (§2) | ✓ Not applicable · no signed tokens, so the whole class is absent by construction |
| Signature before claims (§3) | ✓ Not applicable · same reason |
| Constant-time comparison | ✓ `constant_time_eq` · length-independent, own tests |
| Fail closed (security §5) | ✓ Refuses to boot with no token configured · a documented divergence from the Folio port, and the right one |
| ✗ tokens in URLs (§6) | ✓ `Authorization` header · OAuth code in the redirect only, one-shot and 10 min |
| Per-principal identity (§7, §8) | ✓ Token → principal · one contractor revoked is one line out of a JSON file |
| Refresh single-use (§5) | ✓ Removed on read, pass or fail |
| Key material from config (§4) | ✓ Env cascade · `PIPELINE_TOKENS_FILE` → `PIPELINE_TOKENS` → `PIPELINE_TOKEN` |
| DCR client cap (§8) | ✓ 256 cap · 7 d TTL · uncapped registration is a memory-exhaustion DoS |
| Rate limiting (security §5) | ✓ `ratelimit.rs` · published budget |
| Auth events audited (§10) | ✓ Logged at INFO with the principal, ✗ debug |

The absence of JWT removes an entire family of the standard's rules. That is
worth stating positively: §1 says format follows the revocation requirement, and
an opaque token against a shared store is exactly what that rule points at.

---

## 3. Finding 1 — access-token lifetime · High

`ACCESS_TOKEN_TTL_MS = 24 h`. The standard caps a browser/user-facing access
token at **≤ 15 min**, paired with refresh rotation.

The code documents why: an in-memory-only token meant every container bounce
forced claude.ai to re-authorize from scratch. That is a real problem, but 24 h
is the wrong fix for it — persistence solved the bounce, and the TTL was
stretched on top of a fix that had already landed.

Consequence: a leaked access token is valid for a day, and revoking the refresh
chain does nothing about it. The revocation story degrades to "wait".

**Fixed.** `ACCESS_TOKEN_TTL_MS` is now 15 min. The rotating refresh grant covers
the gap, `expires_in` is published, and persistence stays.

---

## 4. Finding 2 — refresh reuse not treated as theft · Medium

The presented refresh token is removed on read whether or not it validates, so
reuse is correctly rejected. But the standard requires more: reuse of a spent
refresh token means the token was stolen, and the response is to **revoke the
entire chain and raise a security event**.

Today a thief who replays a spent token gets `invalid_grant` — and the legitimate
holder, whose token was rotated out from under them, gets the same. Neither the
operator nor the user learns that a replay happened.

**Fixed.** Grants carry a chain id; rotation inherits it, fresh authorization
starts a new one. A replayed spent token revokes every live grant in that
lineage and logs `event = refresh_token_reuse`. The caller still gets a generic
`invalid_grant`, per §3. The spent ledger is bounded (4096) — an unbounded one
is the memory-exhaustion shape the DCR cap already guards against.

Three tests: a replay kills the rotated-to access token, rotation stays in one
lineage while fresh authorization starts its own, and revoking one chain leaves
another principal's alive.

---

## 5. Finding 3 — redaction is per call site · Low

§6 requires token values redacted **at the logging boundary**, ✗ by remembering
to omit them at each call site. Pipeline logs principals rather than token
values, which is correct today, but nothing structurally prevents a future
`tracing` call from formatting a header or a grant record.

Fix: a redacting wrapper type for token values whose `Debug`/`Display` print a
placeholder, used everywhere a token is held. Then omission is the default and
leaking one requires deliberately unwrapping it.

---

## 6. Not findings

| Observation | Why it is fine |
|---|---|
| No `aud` · `iss` · `jti` validation | Those are JWT claims · opaque tokens carry none |
| No JWKS · no `kid` | No asymmetric signing · nothing to publish |
| Shared `PIPELINE_TOKEN` mode exists | Documented as single-principal · multi-token mode is the multi-user path |
| OAuth redirect wildcard | claude.ai's redirect URI varies by workspace · scoped to that host, documented at the call site |
| 30 d refresh TTL | Standard sets no ceiling for refresh tokens · rotation and reuse detection are the controls, and finding 2 covers the gap |

---

## 7. Order

1. ~~Finding 1~~ — done.
2. ~~Finding 2~~ — done.
3. Finding 3 — open · hardening against a leak that has not happened.
