//! Event Juggler: events that watch a log type, and actions that run when one fires. Only the
//! HTTP action is simulated. It sends `http_text` to `http_url`, with `%et` replaced by the
//! log type and `%ex` by the event text.

use std::net::IpAddr;
use std::time::Duration;

use axum::http::Method;
use serde_json::{Value, json};

use crate::api::{ApiError, ApiResult, Call, ConfigList, Item, item_id};

const EVENTS: &str = "/event_juggler/events/config";
const OPERATIONS: &str = "/event_juggler/operations/config";

#[derive(Default)]
pub struct Juggler {
    events: Vec<Item>,
    operations: ConfigList,
    next_id: u32,
    outbox: Vec<Hook>,
    /// Local address the hooks are sent from; `None` lets the OS choose. Tests that run
    /// several mocks on one machine give each its own loopback address, so the panel can
    /// tell them apart.
    pub source: Option<IpAddr>,
}

/// An HTTP action that fired and still has to be sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Hook {
    pub url: String,
    pub post: bool,
    pub body: String,
    pub source: Option<IpAddr>,
}

impl Hook {
    pub async fn send(self) {
        let client = reqwest::Client::builder()
            .local_address(self.source)
            .build()
            .expect("HTTP client");
        let req = if self.post {
            client.post(&self.url)
        } else {
            client.get(&self.url)
        };
        if let Err(e) = req
            .body(self.body)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            eprintln!("event juggler: {}: {e}", self.url);
        }
    }
}

impl Juggler {
    /// Serves the Event Juggler endpoints, or returns `None` for any other path.
    pub fn call(&mut self, call: &Call) -> Option<ApiResult> {
        if call.is(Method::GET, EVENTS) {
            return Some(Ok(Value::Array(
                self.events.iter().cloned().map(Value::Object).collect(),
            )));
        }
        if call.is(Method::POST, EVENTS) {
            let Value::Object(data) = call.data else {
                return Some(Err(ApiError::bad_data("data must be an object")));
            };
            return Some(Ok(Value::Object(self.create(data.clone()))));
        }
        if *call.method == Method::DELETE
            && let Some(id) = item_id(call.path, EVENTS)
        {
            return Some(self.delete(&id));
        }
        self.operations
            .route(call, OPERATIONS)
            .map(|r| r.map(|(reply, _)| reply))
    }

    /// Creating an event also creates its first action, as on the devices.
    fn create(&mut self, data: Item) -> Item {
        let id = self.take_id();
        let action = self.take_id();
        self.operations.items.push(
            json!({ "id": action, ".type": "operation", "enabled": "1", "plugin": "" })
                .as_object()
                .unwrap()
                .clone(),
        );
        let mut event = Item::new();
        event.insert("enabled".into(), "1".into());
        event.extend(data);
        event.insert("id".into(), id.into());
        event.insert(".type".into(), "event".into());
        event.insert("actions".into(), json!([action]));
        self.events.push(event.clone());
        event
    }

    fn delete(&mut self, id: &str) -> ApiResult {
        let i = self
            .events
            .iter()
            .position(|e| e["id"] == id)
            .ok_or_else(|| ApiError::section_not_found(id))?;
        let event = self.events.remove(i);
        let actions = event["actions"].as_array().cloned().unwrap_or_default();
        self.operations
            .items
            .retain(|o| !actions.contains(&o["id"]));
        Ok(json!({ "id": id }))
    }

    fn take_id(&mut self) -> String {
        self.next_id += 1;
        self.next_id.to_string()
    }

    /// Creates the panel_* rules that `teltonika-panel setup-hooks` creates, for a stack that
    /// works without that step. `rules` holds pairs of rule name and log type.
    pub fn seed(&mut self, hook_url: &str, rules: &[(&str, &str)]) {
        for (name, log_event) in rules {
            let data = json!({ "name": name, "plugin": "log", "log_event": log_event, "log_event_mark": "all" });
            let event = self.create(data.as_object().unwrap().clone());
            let action = event["actions"][0].clone();
            let op = self
                .operations
                .items
                .iter_mut()
                .find(|o| o["id"] == action)
                .unwrap();
            op.extend(
                json!({ "plugin": "http", "http_post": "1", "http_url": hook_url, "http_text": "%et: %ex", "http_timeout": "5" })
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
    }

    /// Runs the HTTP actions of every enabled event that watches `log_event`.
    pub fn fire(&mut self, log_event: &str, text: &str) {
        for e in self
            .events
            .iter()
            .filter(|e| e["enabled"] == "1" && e["log_event"] == log_event)
        {
            let actions = e["actions"].as_array().cloned().unwrap_or_default();
            for op in self
                .operations
                .items
                .iter()
                .filter(|o| actions.contains(&o["id"]))
            {
                let (Some("http"), Some(url)) = (
                    op["plugin"].as_str(),
                    op.get("http_url").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let template = op.get("http_text").and_then(Value::as_str).unwrap_or("");
                self.outbox.push(Hook {
                    url: url.into(),
                    post: op.get("http_post").and_then(Value::as_str) == Some("1"),
                    body: template.replace("%et", log_event).replace("%ex", text),
                    source: self.source,
                });
            }
        }
    }

    /// The hooks fired since the last call.
    pub fn take_hooks(&mut self) -> Vec<Hook> {
        std::mem::take(&mut self.outbox)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(j: &mut Juggler, method: Method, path: &str, data: Value) -> ApiResult {
        j.call(&Call {
            method: &method,
            path,
            data: &data,
        })
        .expect("routed")
    }

    #[test]
    fn creating_an_event_creates_its_action_and_deleting_removes_both() {
        let mut j = Juggler::default();
        let event = call(
            &mut j,
            Method::POST,
            EVENTS,
            json!({ "name": "x", "plugin": "log", "log_event": "Switch Events" }),
        )
        .unwrap();
        assert_eq!(event["enabled"], "1");
        let action = event["actions"][0].as_str().unwrap().to_string();
        assert_ne!(event["id"], action);

        let ops = call(&mut j, Method::GET, OPERATIONS, json!({})).unwrap();
        assert_eq!(ops[0]["id"], action);
        assert_eq!(ops[0]["plugin"], "");

        let id = event["id"].as_str().unwrap();
        assert_eq!(
            call(&mut j, Method::DELETE, &format!("{EVENTS}/{id}"), json!({})).unwrap(),
            json!({ "id": id })
        );
        assert_eq!(
            call(&mut j, Method::GET, EVENTS, json!({})).unwrap(),
            json!([])
        );
        assert_eq!(
            call(&mut j, Method::GET, OPERATIONS, json!({})).unwrap(),
            json!([])
        );
        let err = call(&mut j, Method::DELETE, &format!("{EVENTS}/{id}"), json!({})).unwrap_err();
        assert_eq!(err.code, 122);
    }

    #[test]
    fn fires_only_enabled_http_actions_for_the_log_type() {
        let mut j = Juggler::default();
        j.seed(
            "http://panel/hook",
            &[
                ("panel_ports", "Switch Events"),
                ("panel_mobile", "Mobile Data"),
            ],
        );
        j.fire("Switch Events", "Link state of port 3 changed to DOWN");
        assert_eq!(
            j.take_hooks(),
            vec![Hook {
                url: "http://panel/hook".into(),
                post: true,
                body: "Switch Events: Link state of port 3 changed to DOWN".into(),
                source: None
            }]
        );
        assert!(j.take_hooks().is_empty(), "the outbox empties once read");

        // A disabled event, and an event whose action is not HTTP, send nothing.
        let events = call(&mut j, Method::GET, EVENTS, json!({})).unwrap();
        let mobile = events
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "panel_mobile")
            .unwrap();
        j.events
            .iter_mut()
            .find(|e| e["id"] == mobile["id"])
            .unwrap()
            .insert("enabled".into(), "0".into());
        call(
            &mut j,
            Method::POST,
            EVENTS,
            json!({ "name": "plain", "log_event": "Switch Events" }),
        )
        .unwrap();
        j.fire("Mobile Data", "Mobile data disconnected");
        j.fire("Switch Events", "x");
        assert_eq!(
            j.take_hooks().len(),
            1,
            "only panel_ports has an HTTP action that is enabled"
        );
    }

    #[test]
    fn other_paths_are_not_routed() {
        let mut j = Juggler::default();
        let data = json!({});
        assert!(
            j.call(&Call {
                method: &Method::GET,
                path: "/ports_settings/config",
                data: &data
            })
            .is_none()
        );
    }
}
