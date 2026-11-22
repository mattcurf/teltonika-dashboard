//! Event Juggler rules that make a device POST to a URL when something happens.
//! The Web API has no push channel (no WebSocket or SSE), so this is how a panel hears
//! about port and mobile data changes without waiting for its next poll.

use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, Error, Result};

/// One rule: a name and the device log event type it watches, from
/// `GET /event_juggler/events/options`, e.g. "Switch Events" or "Mobile Data".
pub struct Rule<'a> {
    pub name: &'a str,
    pub log_event: &'a str,
}

/// Deletes the rules whose names start with `prefix`, then creates `rules`. Each rule posts
/// "%et: %ex" (event type and text, e.g. "Switch Events: Port LAN2 link state changed to UP")
/// to `hook_url`. Returns a line per change made.
pub async fn replace_rules(
    c: &Client,
    prefix: &str,
    rules: &[Rule<'_>],
    hook_url: &str,
) -> Result<Vec<String>> {
    let mut log = Vec::new();
    let existing = c
        .call(Method::GET, "/event_juggler/events/config", None)
        .await?;
    for e in existing.as_array().into_iter().flatten() {
        let name = e.get("name").and_then(Value::as_str).unwrap_or_default();
        let id = e.get("id").and_then(Value::as_str).unwrap_or_default();
        if name.starts_with(prefix) && !id.is_empty() {
            c.call(
                Method::DELETE,
                &format!("/event_juggler/events/config/{id}"),
                None,
            )
            .await?;
            log.push(format!("deleted old rule {name}"));
        }
    }
    for rule in rules {
        // Creating an event also creates its first action, which is then set to an HTTP POST.
        let event = c
            .call(
                Method::POST,
                "/event_juggler/events/config",
                Some(json!({ "name": rule.name, "enabled": "1", "plugin": "log", "log_event": rule.log_event, "log_event_mark": "all" })),
            )
            .await?;
        let action = event
            .pointer("/actions/0")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Api(format!("rule {} has no action", rule.name)))?;
        c.call(
            Method::PUT,
            &format!("/event_juggler/operations/config/{action}"),
            Some(json!({ "plugin": "http", "http_post": "1", "http_url": hook_url, "http_ui_params": "0", "http_text": "%et: %ex", "http_timeout": "5" })),
        )
        .await?;
        log.push(format!(
            "created {} ({}) -> POST {hook_url}",
            rule.name, rule.log_event
        ));
    }
    Ok(log)
}
