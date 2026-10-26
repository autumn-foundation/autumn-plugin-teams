# ADR 0001: Own Bot Framework JWT check

- Status: Accepted
- Date: 2026-10-26

## Context

The Bot Connector sends each activity with `Authorization: Bearer <JWT>`.
Microsoft lists the checks: issuer `https://api.botframework.com`, audience =
app ID, time window with 5 min skew, a key from the Bot Framework OpenID key
set, the key endorsement for the activity `channelId`, and a `serviceurl`
claim equal to the activity `serviceUrl`.

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
- The key cache rules are in the verified core: refresh at least every 24 h
  (`keys_fresh`), and at most one load per `min_refresh_secs` for an unknown
  `kid` (`may_refresh_keys`). Unknown `kid` values cannot flood the key
  endpoint.
- RS256 is fixed in code. The token header cannot pick `none` or HS256.

## Results

- Good: the check runs before the body parse. The activity check runs before
  any handler.
- Good: `TestAuthority` signs tokens with a test key, so app tests need no
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
- **Status codes:** 401 for a bad token, 403 for a valid token that does not
  match the activity, 503 when no key set can be loaded (the Connector can
  retry).

## Upstream seams (proposed)

1. `AppBuilder::csrf_exempt_path(path)` for plugins.
2. A public, cached JWKS verifier in autumn-web.
