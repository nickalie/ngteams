use serde_json::{Value, json};

use super::{Caller, repo::Member};
use crate::app::App;

pub(super) async fn publish(app: &App, caller: &Caller, members: &[Member], kind: &str, resource: &Value) {
    let payload = json!({"resourceType": kind, "resource": resource}).to_string();
    let include_actor = kind != "ReadReceipt" && resource["messagetype"].as_str() != Some("Control/Typing");
    for member in members
        .iter()
        .filter(|member| include_actor || member.user_id != caller.id)
    {
        if app
            .fanout
            .publish(&format!("push:8:orgid:{}", member.user_id), payload.clone().into())
            .await
            .is_err()
        {
            tracing::warn!("chat event fan-out unavailable; conversation sync will catch up");
        }
    }
}

pub(super) async fn own(app: &App, caller: &Caller, kind: &str, resource: &Value) {
    let payload = json!({"resourceType": kind, "resource": resource}).to_string();
    if app
        .fanout
        .publish(&format!("push:{}", caller.mri()), payload.into())
        .await
        .is_err()
    {
        tracing::warn!("own chat event fan-out unavailable; conversation sync will catch up");
    }
}
