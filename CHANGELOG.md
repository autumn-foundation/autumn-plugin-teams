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
- Verus proves the retry rules: 429 and 412 wait for `Retry-After`, 5xx and
  network errors retry only for GET, PUT, and DELETE, and a 401 gets one
  token refresh per call.
- The key set refreshes in the background while known keys serve. Old keys
  serve for at most 5 days after a failed refresh.
- `testing` feature: `TestAuthority` and inbound activity builders.
