# ADR 0001: Own Bot Framework JWT check

- Status: Accepted
- Date: 2026-10-26

## Context

The Bot Connector sends each activity with `Authorization: Bearer <JWT>`.
Microsoft lists these checks:

- The issuer is `https://api.botframework.com`.
- The audience is the app ID.
- The JWT is in its time window, with 5 min skew.
- A key from the Bot Framework `OpenID` key set signs it.
- The key endorses the activity `channelId`.
- The `serviceurl` claim equals the activity `serviceUrl`.

autumn-web 0.8 has an OIDC ID token check in `auth.rs`. It is private. It
checks a nonce and other ID token claims. It fetches the key set for each
call. It does not read `endorsements` or `serviceurl`.

## Options

1. Use the Bot Framework SDK. There is no Rust SDK.
2. Ask autumn for a public JWKS verifier. Wait for it.
3. Write the check in the plugin with `jsonwebtoken` 11 (`rust_crypto`), the
   same crate and crypto backend as autumn.

## Decision

Option 3.

## Reasons

- No new crypto backend in the dependency tree.
- The plugin checks the time window with the autumn `ClockSource`, through
  the verified `policy::token_time_valid`. Tests use a fixed clock.
- The verified core holds the key cache rules. `keys_fresh` makes the
  plugin refresh the keys every 24 h or sooner. `may_refresh_keys` allows one
  refresh per `min_refresh_secs` for an unknown `kid`. So unknown `kid`
  values cannot flood the key endpoint.
- A known `kid` serves from the old set while one background task refreshes.
  While the set is less than 5 days old, a refresh never blocks a request
  that has a known key.
- After a failed refresh, the old keys serve until they are 5 days old,
  counted from the last good refresh (`MAX_STALE_KEY_AGE_SECS`). Then the
  route acks with 503.
- RS256 is fixed in code. The JWT header cannot pick `none` or HS256.

## Results

- Good: the JWT check runs before the plugin reads the body. The activity check runs
  before any handler.
- Bad: an attacker can use up the unknown-`kid` refresh just before Microsoft
  starts to use a new key. Real activities can then get 401 for up to
  `min_refresh_secs` (default 60 s).
- Good: `TestAuthority` signs JWTs with a test key, so app tests need no
  Microsoft account.
- Bad: two JWT checks exist in an app that also uses autumn OIDC.
- Bad: the Bot Framework Emulator path (Entra issuers) is not in scope.

## Related decisions

- **One `Route`**, `POST {base}/messages`. autumn stops at boot when an app
  has no `Route`. The base path is a builder setting, because autumn mounts
  routes before it loads config.
- **CSRF:** a plugin cannot add an exemption. The plugin checks at startup
  and fails with the config fix. CAPTCHA checks only form bodies, and the
  Connector sends JSON.
- **Start in a state initializer**, so a `#[job]` gets the client. A startup
  hook reports a start error.
- **Status codes:** 401 for a bad JWT, 403 for a valid JWT that does not
  match the activity, 503 when no key set can be loaded (the Connector can
  send the activity again).

## Upstream seams (proposed)

1. `AppBuilder::csrf_exempt_path(path)` for plugins.
2. A public, cached JWKS verifier in autumn-web.
