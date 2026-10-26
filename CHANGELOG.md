# Changelog

This file lists all notable changes to this project. The format follows
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
- `TeamsClient`: send, reply, update, delete, create conversation, members
  (paged and all), team details and channels, and proactive messages to a
  stored `ConversationReference`. `TeamsContext` adds `reply`, `send`,
  `typing`, `update`, and `delete` for the current conversation.
- Handlers for `messageUpdate` and `messageDelete`.
- Universal Action replies keep HTTP 200. An error goes in the body
  (`InvokeResponse::card_error`).
- `Attachment::with_preview`, `InvokeResponse::compose_message`,
  `InvokeResponse::with_task_size`, `ConversationParameters::channel`, and
  `Activity::is_card_submit`.
- Client-credentials app token for single-tenant and multi-tenant bots, with
  a cache and one request at a time. Callers that wait share the result.
- Verus proves the retry decision and a model of the whole retry loop: a
  rate limit waits for `Retry-After`, 5xx and network errors retry only for
  idempotent calls, and a 401 gets one app token refresh per call. Tests map
  412 and 429 to a rate limit, and POST to not idempotent.
- The key set refreshes in the background while known keys serve. After a
  failed refresh, old keys serve until they are 5 days old.
- Dedup of repeat activities, drain at shutdown, a health indicator,
  Prometheus metrics, and a `serviceUrl` host allowlist for the app token.
- `testing` feature: `TestAuthority` and inbound activity builders.
