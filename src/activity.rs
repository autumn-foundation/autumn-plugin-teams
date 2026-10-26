//! Bot Framework activities and Teams payloads.
//!
//! Field names follow the Bot Framework JSON (camel case). Unknown fields
//! stay in `extra`, so a round trip loses no data.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Activity type names.
pub mod types {
    /// A user message.
    pub const MESSAGE: &str = "message";
    /// A user edited a message.
    pub const MESSAGE_UPDATE: &str = "messageUpdate";
    /// A user deleted a message.
    pub const MESSAGE_DELETE: &str = "messageDelete";
    /// Members or conversation properties changed.
    pub const CONVERSATION_UPDATE: &str = "conversationUpdate";
    /// A user added or removed a reaction.
    pub const MESSAGE_REACTION: &str = "messageReaction";
    /// The app was installed or removed.
    pub const INSTALLATION_UPDATE: &str = "installationUpdate";
    /// A named event.
    pub const EVENT: &str = "event";
    /// A request that needs a reply in the HTTP body.
    pub const INVOKE: &str = "invoke";
    /// A typing indicator.
    pub const TYPING: &str = "typing";
}

/// The invoke name of an Adaptive Card Universal Action.
pub const ADAPTIVE_CARD_ACTION: &str = "adaptiveCard/action";
/// The content type of an Adaptive Card attachment.
pub const ADAPTIVE_CARD_CONTENT_TYPE: &str = "application/vnd.microsoft.card.adaptive";

/// A user or bot account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelAccount {
    /// Channel user ID, for example `29:1abc...`.
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Entra object ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aad_object_id: Option<String>,
    /// `user` or `bot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Other fields.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ChannelAccount {
    /// Makes an account with this ID and name.
    #[must_use]
    pub fn new(id: impl Into<String>, name: Option<String>) -> Self {
        Self {
            id: id.into(),
            name,
            ..Self::default()
        }
    }
}

/// A conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ConversationAccount {
    /// Conversation ID.
    pub id: String,
    /// Name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `personal`, `groupChat`, or `channel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_type: Option<String>,
    /// Tenant ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// `true` for a group chat or a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_group: Option<bool>,
    /// Other fields.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ConversationAccount {
    /// Makes a conversation with this ID.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }
}

/// An attachment, for example an Adaptive Card.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Attachment {
    /// MIME type of the content.
    pub content_type: String,
    /// Inline content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    /// URL of the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    /// File name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Preview card, for a message extension result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Box<Self>>,
    /// Other fields.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Attachment {
    /// An Adaptive Card attachment.
    #[must_use]
    pub fn adaptive_card(card: Value) -> Self {
        Self {
            content_type: ADAPTIVE_CARD_CONTENT_TYPE.to_owned(),
            content: Some(card),
            ..Self::default()
        }
    }

    /// Sets the preview card of a message extension result.
    #[must_use]
    pub fn with_preview(mut self, preview: Self) -> Self {
        self.preview = Some(Box::new(preview));
        self
    }

    /// An attachment with this content type and content.
    #[must_use]
    pub fn new(content_type: impl Into<String>, content: Value) -> Self {
        Self {
            content_type: content_type.into(),
            content: Some(content),
            ..Self::default()
        }
    }
}

/// A reaction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MessageReaction {
    /// Reaction type, for example `like`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// A Bot Framework activity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Activity {
    /// Activity type. See [`types`].
    #[serde(rename = "type")]
    pub kind: String,
    /// Activity ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// UTC time, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Connector endpoint for replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_url: Option<String>,
    /// Channel, `msteams` for Teams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ChannelAccount>,
    /// Conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationAccount>,
    /// Receiver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<ChannelAccount>,
    /// Message text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `plain`, `markdown`, or `xml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_format: Option<String>,
    /// Text for a notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `list` or `carousel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_layout: Option<String>,
    /// Attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Entities, for example mentions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Value>,
    /// Teams channel data: tenant, team, channel, event type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_data: Option<Value>,
    /// The activity that this activity replies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<String>,
    /// Value: a card submit, or an invoke payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Name of an invoke or an event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `add`, `remove`, `add-upgrade`, or `remove-upgrade`, for
    /// `installationUpdate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Members added, for `conversationUpdate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members_added: Vec<ChannelAccount>,
    /// Members removed, for `conversationUpdate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members_removed: Vec<ChannelAccount>,
    /// Reactions added, for `messageReaction`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions_added: Vec<MessageReaction>,
    /// Reactions removed, for `messageReaction`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions_removed: Vec<MessageReaction>,
    /// Locale, for example `en-US`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Other fields.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Activity {
    /// A message with this text.
    #[must_use]
    pub fn message(text: impl Into<String>) -> Self {
        Self {
            kind: types::MESSAGE.to_owned(),
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// A typing indicator.
    #[must_use]
    pub fn typing() -> Self {
        Self {
            kind: types::TYPING.to_owned(),
            ..Self::default()
        }
    }

    /// Adds an attachment.
    #[must_use]
    pub fn with_attachment(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// Sets the text format, for example `markdown`.
    #[must_use]
    pub fn with_text_format(mut self, format: impl Into<String>) -> Self {
        self.text_format = Some(format.into());
        self
    }

    /// Adds `<at>name</at>` before the text and a mention entity, so Teams
    /// notifies `account`.
    #[must_use]
    pub fn with_mention(mut self, account: &ChannelAccount) -> Self {
        let name = account.name.as_deref().unwrap_or(&account.id);
        let tag = format!("<at>{}</at>", xml_escape(name));
        self.text = Some(
            self.text
                .take()
                .filter(|t| !t.is_empty())
                .map_or_else(|| tag.clone(), |t| format!("{tag} {t}")),
        );
        self.entities.push(json!({
            "type": "mention",
            "text": tag,
            "mentioned": { "id": account.id, "name": name },
        }));
        self
    }

    /// Returns `true` for an invoke.
    #[must_use]
    pub fn is_invoke(&self) -> bool {
        self.kind == types::INVOKE
    }

    /// The text with all `<at>...</at>` mentions removed and trimmed.
    #[must_use]
    pub fn text_without_mentions(&self) -> String {
        let mut rest = self.text.as_deref().unwrap_or_default();
        let mut out = String::with_capacity(rest.len());
        while let Some(open) = rest.find("<at>") {
            let Some(close) = rest[open..].find("</at>") else {
                break;
            };
            out.push_str(&rest[..open]);
            rest = &rest[open + close + "</at>".len()..];
        }
        out.push_str(rest);
        out.trim().to_owned()
    }

    /// The text without the mention of the bot (the recipient), trimmed.
    /// Other mentions stay. With no mention entity, only a leading
    /// `<at>...</at>` is removed.
    #[must_use]
    pub fn text_without_bot_mention(&self) -> String {
        let text = self.text.as_deref().unwrap_or_default();
        let bot = self.recipient.as_ref().map(|r| r.id.as_str());
        let tags: Vec<&str> = self
            .entities
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("mention"))
            .filter(|e| e.pointer("/mentioned/id").and_then(Value::as_str) == bot)
            .filter_map(|e| e.get("text").and_then(Value::as_str))
            .collect();
        if bot.is_some() && !tags.is_empty() {
            let mut out = text.to_owned();
            for tag in tags {
                out = out.replace(tag, "");
            }
            return out.trim().to_owned();
        }
        let trimmed = text.trim_start();
        let rest = trimmed
            .strip_prefix("<at>")
            .and_then(|r| r.find("</at>").map(|i| &r[i + "</at>".len()..]))
            .unwrap_or(trimmed);
        rest.trim().to_owned()
    }

    /// Returns `true` for a card `Action.Submit`: a message with a `value`
    /// and no text.
    #[must_use]
    pub fn is_card_submit(&self) -> bool {
        self.kind == types::MESSAGE
            && self.value.is_some()
            && self.text.as_deref().is_none_or(|t| t.trim().is_empty())
    }

    /// The first word of [`Self::text_without_bot_mention`], in lower case
    /// and with no leading `/`, and the rest of the text.
    #[must_use]
    pub fn command(&self) -> Option<(String, String)> {
        let text = self.text_without_bot_mention();
        let (word, rest) = text.split_once(char::is_whitespace).unwrap_or((&text, ""));
        let word = word.trim_start_matches('/').to_lowercase();
        (!word.is_empty()).then(|| (word, rest.trim().to_owned()))
    }

    /// Tenant ID from `channelData.tenant.id`, else `conversation.tenantId`.
    #[must_use]
    pub fn tenant_id(&self) -> Option<&str> {
        self.channel_data_str(&["tenant", "id"]).or_else(|| {
            self.conversation
                .as_ref()
                .and_then(|c| c.tenant_id.as_deref())
        })
    }

    /// Team ID from `channelData.team.id`.
    #[must_use]
    pub fn team_id(&self) -> Option<&str> {
        self.channel_data_str(&["team", "id"])
    }

    /// Teams event type from `channelData.eventType`, for example
    /// `channelCreated`. Compare without case: Teams is not consistent.
    #[must_use]
    pub fn event_type(&self) -> Option<&str> {
        self.channel_data_str(&["eventType"])
    }

    /// The string at `path` in `channelData`.
    fn channel_data_str(&self, path: &[&str]) -> Option<&str> {
        path.iter()
            .try_fold(self.channel_data.as_ref()?, |v, k| v.get(k))?
            .as_str()
    }

    /// Members added, without the bot itself (the recipient).
    #[must_use]
    pub fn members_added_except_bot(&self) -> Vec<&ChannelAccount> {
        let bot = self.recipient.as_ref().map(|r| r.id.as_str());
        self.members_added
            .iter()
            .filter(|m| Some(m.id.as_str()) != bot)
            .collect()
    }

    /// The Adaptive Card Universal Action of an `adaptiveCard/action` invoke.
    #[must_use]
    pub fn card_action(&self) -> Option<CardAction> {
        if self.name.as_deref() != Some(ADAPTIVE_CARD_ACTION) {
            return None;
        }
        let action = self.value.as_ref()?.get("action")?;
        CardAction::deserialize(action).ok()
    }

    /// A reference to this conversation, for replies and proactive messages.
    /// `None` when the activity has no `serviceUrl` or no conversation.
    #[must_use]
    pub fn conversation_reference(&self) -> Option<ConversationReference> {
        Some(ConversationReference {
            activity_id: self.id.clone(),
            user: self.from.clone(),
            bot: self.recipient.clone(),
            conversation: self.conversation.clone()?,
            channel_id: self.channel_id.clone().unwrap_or_default(),
            service_url: self.service_url.clone()?,
            locale: self.locale.clone(),
        })
    }
}

/// Escapes `&`, `<`, and `>` for Teams message text.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// An Adaptive Card Universal Action (`Action.Execute`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct CardAction {
    /// The verb of the action.
    pub verb: String,
    /// The data of the action and the card inputs.
    #[serde(default)]
    pub data: Value,
}

/// The parts of a conversation that a bot needs to send to it later. It is
/// `Serialize` and `Deserialize`: store it, then use it with
/// [`crate::TeamsClient::send_to`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ConversationReference {
    /// The activity that the reference came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
    /// The user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ChannelAccount>,
    /// The bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<ChannelAccount>,
    /// The conversation.
    pub conversation: ConversationAccount,
    /// Channel, `msteams` for Teams.
    pub channel_id: String,
    /// Connector endpoint.
    pub service_url: String,
    /// Locale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// Parameters for a new conversation. See
/// [`crate::TeamsClient::create_conversation`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ConversationParameters {
    /// `true` for a group conversation.
    #[serde(default)]
    pub is_group: bool,
    /// The bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<ChannelAccount>,
    /// Members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<ChannelAccount>,
    /// Topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_name: Option<String>,
    /// Tenant ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// First activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    /// Channel data. Teams needs `{"tenant": {"id": ...}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_data: Option<Value>,
}

impl ConversationParameters {
    /// A 1:1 conversation between `bot` and `user` in `tenant_id`.
    #[must_use]
    pub fn personal(bot: ChannelAccount, user: ChannelAccount, tenant_id: &str) -> Self {
        Self {
            is_group: false,
            bot: Some(bot),
            members: vec![user],
            tenant_id: Some(tenant_id.to_owned()),
            channel_data: Some(json!({ "tenant": { "id": tenant_id } })),
            ..Self::default()
        }
    }
}

/// A Connector reply with a resource ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ResourceResponse {
    /// Resource ID, for example the new activity ID.
    #[serde(default)]
    pub id: String,
}

/// The reply to `create_conversation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ConversationResourceResponse {
    /// Conversation ID.
    #[serde(default)]
    pub id: String,
    /// ID of the first activity, if one was sent.
    #[serde(default)]
    pub activity_id: Option<String>,
    /// Connector endpoint for the conversation.
    #[serde(default)]
    pub service_url: Option<String>,
}

/// A Teams member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TeamsChannelAccount {
    /// Channel user ID.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Entra object ID.
    #[serde(default)]
    pub aad_object_id: Option<String>,
    /// Email.
    #[serde(default)]
    pub email: Option<String>,
    /// User principal name.
    #[serde(default)]
    pub user_principal_name: Option<String>,
    /// Given name.
    #[serde(default)]
    pub given_name: Option<String>,
    /// Surname.
    #[serde(default)]
    pub surname: Option<String>,
    /// Tenant ID.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// `user`, `guest`, or `anonymous`.
    #[serde(default)]
    pub user_role: Option<String>,
}

/// One page of members.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PagedMembers {
    /// Members on this page.
    #[serde(default)]
    pub members: Vec<TeamsChannelAccount>,
    /// Token for the next page. `None` on the last page.
    #[serde(default)]
    pub continuation_token: Option<String>,
}

/// Team details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TeamDetails {
    /// Team ID.
    pub id: String,
    /// Team name.
    #[serde(default)]
    pub name: Option<String>,
    /// Entra group ID.
    #[serde(default)]
    pub aad_group_id: Option<String>,
}

/// A channel in a team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ChannelInfo {
    /// Channel ID (a conversation ID).
    pub id: String,
    /// Channel name. `None` for the General channel.
    #[serde(default)]
    pub name: Option<String>,
}

/// The HTTP reply to an invoke.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InvokeResponse {
    /// HTTP status.
    pub status: u16,
    /// JSON body, if any.
    pub body: Option<Value>,
}

impl InvokeResponse {
    /// 200 with no body.
    #[must_use]
    pub const fn ok() -> Self {
        Self {
            status: 200,
            body: None,
        }
    }

    /// This status and JSON body.
    #[must_use]
    pub const fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: Some(body),
        }
    }

    /// Universal Action reply: Teams shows this text.
    #[must_use]
    pub fn card_message(text: impl Into<String>) -> Self {
        Self::json(
            200,
            json!({
                "statusCode": 200,
                "type": "application/vnd.microsoft.activity.message",
                "value": text.into(),
            }),
        )
    }

    /// Universal Action reply: Teams replaces the card with `card`.
    #[must_use]
    pub fn card(card: Value) -> Self {
        let mut body = json!({ "statusCode": 200, "type": ADAPTIVE_CARD_CONTENT_TYPE });
        body["value"] = card;
        Self::json(200, body)
    }

    /// Task module reply: Teams shows this text.
    #[must_use]
    pub fn task_message(text: impl Into<String>) -> Self {
        Self::json(
            200,
            json!({ "task": { "type": "message", "value": text.into() } }),
        )
    }

    /// Task module reply: Teams opens a dialog with this title and card.
    #[must_use]
    pub fn task_card(title: impl Into<String>, card: Value) -> Self {
        let mut body = json!({ "task": { "type": "continue", "value": {
            "title": title.into(),
            "card": { "contentType": ADAPTIVE_CARD_CONTENT_TYPE },
        } } });
        body["task"]["value"]["card"]["content"] = card;
        Self::json(200, body)
    }

    /// Universal Action error reply. Teams needs HTTP 200; the error is in
    /// the body. `status` is 400 or 500.
    #[must_use]
    pub fn card_error(status: u16, message: impl Into<String>) -> Self {
        Self::json(
            200,
            json!({
                "statusCode": status,
                "type": "application/vnd.microsoft.error",
                "value": { "code": status.to_string(), "message": message.into() },
            }),
        )
    }

    /// Message extension reply: Teams shows this text.
    #[must_use]
    pub fn compose_message(text: impl Into<String>) -> Self {
        Self::json(
            200,
            json!({ "composeExtension": { "type": "message", "text": text.into() } }),
        )
    }

    /// Message extension reply: a list of result attachments. An Adaptive
    /// Card result needs a preview: see [`Attachment::with_preview`].
    #[must_use]
    pub fn compose_results(attachments: &[Attachment]) -> Self {
        Self::json(
            200,
            json!({ "composeExtension": {
                "type": "result",
                "attachmentLayout": "list",
                "attachments": attachments,
            } }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> Activity {
        let mut a = Activity::message(text);
        a.service_url = Some("https://smba.trafficmanager.net/teams/".into());
        a.channel_id = Some("msteams".into());
        a.id = Some("a1".into());
        a.from = Some(ChannelAccount::new("29:u", Some("Ada".into())));
        a.recipient = Some(ChannelAccount::new("28:bot", Some("Bot".into())));
        a.conversation = Some(ConversationAccount::new("19:c"));
        a
    }

    #[test]
    fn mentions_and_commands() {
        let a = msg("<at>Bot</at>  Deploy   prod <at>Ada</at> now ");
        assert_eq!(a.text_without_mentions(), "Deploy   prod  now");
        // Only the bot mention goes: other mentions are arguments.
        assert_eq!(
            a.command(),
            Some(("deploy".to_owned(), "prod <at>Ada</at> now".to_owned()))
        );
        assert_eq!(msg("/Help").command(), Some(("help".into(), String::new())));
        assert_eq!(msg("<at>Bot</at>").command(), None);
        assert_eq!(msg("   ").command(), None);
        // An open tag with no close tag stays as text.
        assert_eq!(msg("a <at>b").text_without_mentions(), "a <at>b");
        let mut none = msg("");
        none.text = None;
        assert_eq!(none.text_without_mentions(), "");
    }

    #[test]
    fn with_mention_adds_text_and_entity() {
        let ada = ChannelAccount::new("29:u", Some("Ada".into()));
        let a = Activity::message("hello").with_mention(&ada);
        assert_eq!(a.text.as_deref(), Some("<at>Ada</at> hello"));
        assert_eq!(a.entities[0]["type"], "mention");
        assert_eq!(a.entities[0]["text"], "<at>Ada</at>");
        assert_eq!(a.entities[0]["mentioned"]["id"], "29:u");
        // No name: the ID is the text.
        let anon = ChannelAccount::new("29:x", None);
        let a = Activity::typing().with_mention(&anon);
        assert_eq!(a.text.as_deref(), Some("<at>29:x</at>"));
    }

    #[test]
    fn channel_data_accessors() {
        let mut a = msg("x");
        assert_eq!(a.tenant_id(), None);
        a.conversation.as_mut().unwrap().tenant_id = Some("t-conv".into());
        assert_eq!(a.tenant_id(), Some("t-conv"));
        a.channel_data = Some(json!({
            "tenant": {"id": "t-cd"},
            "team": {"id": "19:team"},
            "eventType": "channelCreated",
        }));
        assert_eq!(a.tenant_id(), Some("t-cd"));
        assert_eq!(a.team_id(), Some("19:team"));
        assert_eq!(a.event_type(), Some("channelCreated"));
        a.channel_data = Some(json!({"tenant": {"id": 5}}));
        assert_eq!(a.tenant_id(), Some("t-conv"));
    }

    #[test]
    fn members_added_except_bot() {
        let mut a = msg("x");
        a.members_added = vec![
            ChannelAccount::new("28:bot", None),
            ChannelAccount::new("29:new", None),
        ];
        let ids: Vec<_> = a
            .members_added_except_bot()
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["29:new"]);
    }

    #[test]
    fn card_action_parse() {
        let mut a = msg("x");
        a.kind = types::INVOKE.into();
        a.name = Some(ADAPTIVE_CARD_ACTION.into());
        a.value =
            Some(json!({"action": {"type": "Action.Execute", "verb": "ok", "data": {"n": 1}}}));
        let c = a.card_action().unwrap();
        assert_eq!(c.verb, "ok");
        assert_eq!(c.data["n"], 1);
        a.name = Some("task/fetch".into());
        assert!(a.card_action().is_none());
        a.name = Some(ADAPTIVE_CARD_ACTION.into());
        a.value = Some(json!({"action": {"type": "Action.Execute"}}));
        assert!(a.card_action().is_none());
    }

    #[test]
    fn conversation_reference_and_round_trip() {
        let a = msg("x");
        let r = a.conversation_reference().unwrap();
        assert_eq!(r.service_url, "https://smba.trafficmanager.net/teams/");
        assert_eq!(r.conversation.id, "19:c");
        assert_eq!(r.bot.as_ref().unwrap().id, "28:bot");
        assert_eq!(r.user.as_ref().unwrap().id, "29:u");
        assert_eq!(r.activity_id.as_deref(), Some("a1"));
        assert_eq!(r.channel_id, "msteams");
        let mut no_url = a;
        no_url.service_url = None;
        assert!(no_url.conversation_reference().is_none());
        // Unknown fields survive a round trip.
        let raw = json!({"type": "message", "text": "t", "newField": {"x": 1}, "attachments": []});
        let a: Activity = serde_json::from_value(raw).unwrap();
        assert_eq!(a.extra["newField"]["x"], 1);
        let back = serde_json::to_value(&a).unwrap();
        assert_eq!(back["newField"]["x"], 1);
        assert!(back.get("attachments").is_none());
        assert!(back.get("id").is_none());
        assert!(serde_json::from_value::<Activity>(json!({"text": "no type"})).is_err());
    }

    #[test]
    fn unknown_fields_survive_in_nested_types() {
        let raw = json!({
            "type": "message",
            "from": {"id": "29:u", "userRole": "guest"},
            "conversation": {"id": "19:c", "tenantId": "t", "isFuture": 1},
            "attachments": [{
                "contentType": ADAPTIVE_CARD_CONTENT_TYPE,
                "content": {},
                "preview": {"contentType": "application/vnd.microsoft.card.thumbnail", "content": {"title": "t"}},
                "thumbnailUrl": "https://x.example/t.png",
            }],
        });
        let a: Activity = serde_json::from_value(raw.clone()).unwrap();
        assert!(a.attachments[0].preview.is_some());
        assert_eq!(serde_json::to_value(&a).unwrap(), raw);
    }

    #[test]
    fn mention_names_are_escaped() {
        let odd = ChannelAccount::new("29:x", Some("A&B <C>".into()));
        let a = Activity::message("hi").with_mention(&odd);
        assert_eq!(a.text.as_deref(), Some("<at>A&amp;B &lt;C&gt;</at> hi"));
        assert_eq!(a.entities[0]["text"], "<at>A&amp;B &lt;C&gt;</at>");
        assert_eq!(a.entities[0]["mentioned"]["name"], "A&B <C>");
    }

    #[test]
    fn bot_mention_only_is_removed_for_commands() {
        // No mention entities: only a leading mention is removed.
        assert_eq!(
            msg("<at>Bot</at> give <at>Ada</at> a hug").command(),
            Some(("give".into(), "<at>Ada</at> a hug".into()))
        );
        // With entities: the bot mention, wherever it is.
        let mut a = msg("please <at>Bot</at> deploy");
        a.entities =
            vec![json!({"type": "mention", "text": "<at>Bot</at>", "mentioned": {"id": "28:bot"}})];
        assert_eq!(a.text_without_bot_mention(), "please  deploy");
        assert_eq!(a.command(), Some(("please".into(), "deploy".into())));
    }

    #[test]
    fn card_submit_and_more_reply_shapes() {
        let mut a = msg("");
        a.text = None;
        a.value = Some(json!({"x": 1}));
        assert!(a.is_card_submit());
        assert!(!msg("hi").is_card_submit());
        let m = InvokeResponse::compose_message("pick one").body.unwrap();
        assert_eq!(
            m,
            json!({"composeExtension": {"type": "message", "text": "pick one"}})
        );
        let e = InvokeResponse::card_error(500, "Try later.");
        assert_eq!(e.status, 200);
        let b = e.body.unwrap();
        assert_eq!(b["statusCode"], 500);
        assert_eq!(b["type"], "application/vnd.microsoft.error");
        assert_eq!(b["value"]["code"], "500");
        assert_eq!(b["value"]["message"], "Try later.");
    }

    #[test]
    fn invoke_reply_shapes() {
        assert_eq!(InvokeResponse::ok().status, 200);
        assert!(InvokeResponse::ok().body.is_none());
        let c = InvokeResponse::card(json!({"type": "AdaptiveCard"}));
        assert_eq!(c.body.unwrap()["type"], ADAPTIVE_CARD_CONTENT_TYPE);
        let t = InvokeResponse::task_message("done").body.unwrap();
        assert_eq!(t["task"]["value"], "done");
        let r = InvokeResponse::compose_results(&[Attachment::new("text/plain", json!("x"))])
            .body
            .unwrap();
        assert_eq!(
            r["composeExtension"]["attachments"][0]["contentType"],
            "text/plain"
        );
        let p = ConversationParameters::personal(
            ChannelAccount::new("28:b", None),
            ChannelAccount::new("29:u", None),
            "t1",
        );
        assert_eq!(serde_json::to_value(&p).unwrap()["tenantId"], "t1");
    }
}
