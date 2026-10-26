# CLAUDE.md

Microsoft Teams plugin for autumn-web 0.8. Style for all docs and comments:
ASD-STE100. Short sentences, active voice, simple present tense.

## Layout

| Path | Contents |
|---|---|
| `src/policy.rs` | Verified core: token window, token refresh, key refresh, backoff, retry decision. Pure. |
| `verus/policy.rs` | Verus spec and proofs for `src/policy.rs`. Make the same change in both files. |
| `src/auth.rs` | Inbound JWT check, key sources, key cache, activity checks. |
| `src/token.rs` | Outbound app token: client credentials, cache, single flight. |
| `src/config.rs` | `[teams]` config, profile file, `AUTUMN_TEAMS__*` env overlay. |
| `src/transport.rs` | `HttpTransport` trait, `ReqwestTransport`, `MemoryTransport` (fake). |
| `src/client.rs` | `TeamsClient`: Connector REST calls, `serviceUrl` allowlist. |
| `src/activity.rs` | Typed activities, references, invoke replies. |
| `src/handlers.rs` | `TeamsContext`, handler registry, task runner. |
| `src/engine.rs` | Request processing: verify, parse, check, dedup, dispatch, ack. |
| `src/routes.rs` | The axum and autumn route. |
| `src/health.rs`, `src/metrics.rs` | Actuator health and Prometheus metrics. |
| `src/plugin.rs` | `TeamsPlugin`, `TeamsRuntime`, startup checks. |
| `src/testing.rs` | `TestAuthority` and inbound activity builders (feature `testing`). |
| `tests/` | `inbound.rs` (TestApp), `client.rs` (fake transport), `runtime.rs` (install, keys, health, drain), `hardening.rs` (review regressions), `http.rs` (real HTTP to a local fake), `boot.rs` (real app boot), `load.rs` (config files). |
| `docs/` | `plan.md`, `ac-evidence.md`, ADRs. |

## Commands

```bash
git config core.hooksPath .githooks   # pre-commit: fmt, clippy, test
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo llvm-cov --all-targets --all-features --summary-only
verus verus/policy.rs
```

## Rules

- Write the test first. See it fail. Then write the code.
- A change to `src/policy.rs` needs the same change in `verus/policy.rs`.
  Run Verus. The test `verus_bodies_match` compares the two.
- Check the JWT before the body is read. Check the activity against the
  JWT (endorsement, `serviceUrl`, tenant) before any handler runs.
- A Universal Action (`adaptiveCard/action`) reply is always HTTP 200. Put
  the error in the body.
- Use these terms: "JWT" for the inbound token, "app token" for the
  outbound token, "ack" for the HTTP reply to the Connector, "reply" for a
  message that the bot sends, "refresh" for a key set load.
- RS256 only. Never let the token header pick the algorithm.
- Never send the app token to a host that is not in `service_url_hosts`.
- Never retry a POST after a 5xx or a network error. It can post two times.
  A 401 retry is safe: the Connector did nothing.
- Only `src/transport.rs` does network I/O. Other code uses `HttpTransport`.
- A metrics label is a fixed value from code. Never activity data.
- Teams never sees error details. Use `error_text`, or a 500 with no body.
- A request must not wait for a refresh when an old value still works (app
  token, known key). Run a shared refresh in its own task, so a cancelled
  caller does not stop it.
- No `unwrap` or `expect` in `src/` outside tests and `testing.rs`.
- No `unsafe`.
- A test that needs env vars runs a child process. See `tests/boot.rs`.
- Delivery is at least once. Do not claim exactly once.
- Do not log bodies, tokens, or secrets.
- Ground autumn API questions in the autumn-web 0.8 sources or the autumn
  docs MCP, never memory.
