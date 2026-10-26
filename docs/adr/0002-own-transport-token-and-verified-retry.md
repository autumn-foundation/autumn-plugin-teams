# ADR 0002: Own transport, app token, and a verified retry policy

- Status: Accepted
- Date: 2026-10-26

## Context

The bot calls the Connector REST API at the activity `serviceUrl`. Each call
needs an app token from the Entra token endpoint (client credentials). Sends
are `POST`. Updates are `PUT`. Deletes are `DELETE`. Reads are `GET`. The
Connector replies 429 when the bot sends too fast. Teams asks to retry 412
like 429. After a 5xx or a network error, the bot cannot know if the
Connector did the call. After a 401, the Connector did nothing.

autumn-web 0.8 has `http_client::Client`. It retries only idempotent HTTP
methods. It has no token cache and no host allowlist.

## Options

1. Use autumn `Client`, plus a token layer.
2. Write a small `HttpTransport` trait. Put the retry rules in a pure,
   verified function (`policy::decide`). Put the token cache in the plugin.

## Decision

Option 2.

## Reasons

- The rule needs a 401 step: get a new token and retry one time. autumn
  `Client` has no such step.
- A pure function is easy to prove. Verus proves: bounded attempts, bounded
  waits, no 5xx or network retry for a POST, one token refresh per call, and
  the exact `Retry-After` wait.
- The token goes only to hosts in `service_url_hosts`. A forged or stored
  `serviceUrl` cannot get the token. The transport follows no redirects.
- The trait gives `MemoryTransport` for tests with no network.

## Results

- Good: no double posts from retries.
- Good: the token cache rules are verified: `token_margin` gives 5 min, or
  half the life of a short token, and `token_needs_refresh` uses it.
- Good: one token request runs at a time, in its own task. Callers that wait
  share its result. The old token serves while a refresh runs. A cancelled
  caller (for example a health timeout) does not stop the request.
- Bad: Microsoft asks to retry 502 and 504. The plugin does not retry them
  for a POST, because a send can post two times.
- Bad: autumn `[http_client]` config and interceptors do not apply to
  Connector calls.

## Ack strategy

- Teams waits 5 s for an invoke reply. The plugin waits `invoke_timeout_ms`
  (max 4.5 s) and then sends a fallback reply. A Universal Action reply is
  always HTTP 200, with any error in the body.
- Other activities have no reply content. The plugin acks at once and runs
  the handler in the background.
- The plugin drops a repeat activity ID per conversation, so a Connector
  retry does not run a handler two times. The dedup check runs in the
  background task: a dropped request cannot leave a stored ID with no run.
  Invokes have no dedup, because each one needs a reply.
- At shutdown, the route replies 503 and the plugin waits for handlers up to
  `drain_timeout_secs`. A restart stops background handlers: use `#[job]` for
  durable work.
