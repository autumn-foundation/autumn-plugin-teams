# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

- `TeamsPlugin`: one-line install for autumn-web 0.8. It mounts
  `POST /teams/messages`, declares `[teams]`, and adds health and metrics.
- Inbound Bot Framework JWT check: RS256, Bot Framework key set by `kid`,
  issuer, audience, time window with skew, key endorsement, `serviceUrl`
  claim, and optional tenant allowlist.
- Handlers by command, message, members added and removed, conversation
  update, reaction, installation update, event name, invoke name, and
  Adaptive Card verb.
- `TeamsClient`: send, reply, update, delete, typing, create conversation,
  members (paged and all), team details and channels, and proactive messages
  to a stored `ConversationReference`.
- Client-credentials app token for single-tenant and multi-tenant bots, with
  cache and single flight.
- Retry rules proven with Verus: 429 waits `Retry-After`, 5xx and network
  errors retry only for GET, PUT, and DELETE, one token refresh on 401.
- `testing` feature: `TestAuthority` and inbound activity builders.
