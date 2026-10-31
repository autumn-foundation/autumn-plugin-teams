//! A Teams bot with a command, a card action, a welcome text, and a
//! proactive message.
//!
//! Run:
//!
//! ```sh
//! TEAMS_APP_ID=... TEAMS_APP_PASSWORD=... cargo run --example app
//! ```
//!
//! Set the Azure Bot messaging endpoint to `https://<host>/teams/messages`.

use std::sync::Mutex;

use autumn_plugin_teams::activity::{Activity, Attachment, ConversationReference, InvokeResponse};
use autumn_plugin_teams::{HandlerResult, TeamsClient, TeamsContext, TeamsPlugin};
use autumn_web::AppState;
use serde_json::json;

/// The last conversation that said `subscribe`. A real app stores it in a
/// database.
static SUBSCRIBER: Mutex<Option<ConversationReference>> = Mutex::new(None);

/// `deploy <env>`: replies with a card that has an Approve button.
async fn deploy(ctx: TeamsContext, activity: Activity) -> HandlerResult<()> {
    let env = activity
        .command()
        .map(|(_, rest)| rest)
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "staging".to_owned());
    let card = json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [{ "type": "TextBlock", "text": format!("Deploy to {env}?") }],
        "actions": [{
            "type": "Action.Execute",
            "title": "Approve",
            "verb": "approve",
            "data": { "env": env },
        }],
    });
    ctx.reply(Activity::message("").with_attachment(Attachment::adaptive_card(card)))
        .await?;
    Ok(())
}

/// The Approve button: Teams replaces the card with the reply card.
async fn approve(_ctx: TeamsContext, activity: Activity) -> HandlerResult<InvokeResponse> {
    let env = activity
        .card_action()
        .and_then(|a| a.data["env"].as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(InvokeResponse::card(json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [{ "type": "TextBlock", "text": format!("Deploy to {env}: approved") }],
    })))
}

/// `subscribe`: stores the conversation for a later proactive message.
async fn subscribe(ctx: TeamsContext, _activity: Activity) -> HandlerResult<()> {
    *SUBSCRIBER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ctx.reference().clone());
    ctx.reply(Activity::message("Subscribed.")).await?;
    Ok(())
}

/// Welcomes each new member, but not the bot itself.
async fn welcome(ctx: TeamsContext, activity: Activity) -> HandlerResult<()> {
    for member in activity.members_added_except_bot() {
        ctx.send(Activity::message("Welcome! Say `deploy prod`.").with_mention(member))
            .await?;
    }
    Ok(())
}

/// Any other message.
async fn help(ctx: TeamsContext, _activity: Activity) -> HandlerResult<()> {
    ctx.reply(Activity::message("Try `deploy prod` or `subscribe`."))
        .await?;
    Ok(())
}

/// A proactive message, for example from a `#[job]`.
#[allow(dead_code)]
async fn notify(state: &AppState, text: &str) -> HandlerResult<()> {
    let reference = SUBSCRIBER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let (Some(client), Some(reference)) = (TeamsClient::from_state(state), reference) {
        client.send_to(&reference, &Activity::message(text)).await?;
    }
    Ok(())
}

#[autumn_web::main]
async fn main() {
    Box::pin(
        autumn_web::app()
            .plugin(
                TeamsPlugin::new()
                    .command("deploy", deploy)
                    .command("subscribe", subscribe)
                    .card_action("approve", approve)
                    .on_members_added(welcome)
                    .on_message(help),
            )
            .run(),
    )
    .await;
}
