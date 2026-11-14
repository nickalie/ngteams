use serde_json::{Value, json};

use super::{
    Caller,
    repo::{Chat, Member},
};
use crate::app::App;

pub(super) fn base(app: &App) -> String {
    format!("{}/api/chatsvc/{}", app.config.origin(), app.config.region)
}

pub(super) fn member(app: &App, member: &Member, ranges: bool) -> Value {
    let mri = format!("8:orgid:{}", member.user_id);
    let mut value = json!({
        "capabilities": ["kickMember"], "cid": "0", "hidden": false, "id": mri,
        "isFollowing": false, "linkedMri": "", "role": "Admin", "type": "ThreadMember",
        "userLink": format!("{}/v1/users/{mri}", base(app)),
    });
    if ranges {
        value["meetingRole"] = json!("None");
        value["rangeStart"] = json!(member.joined_at);
        value["rangeEnd"] = json!(253_402_300_800_000_i64);
    } else {
        value["interest"] = json!("Interested");
    }
    value
}

pub(super) fn thread(app: &App, caller: &Caller, chat: &Chat, members: &[Member]) -> Value {
    let mut properties = json!({
        "capabilities": ["AddMember", "ChangeTopic", "ChangePicture", "EditMsg", "SendText"],
        "createdat": chat.created_at.to_string(), "creator": format!("8:orgid:{}", chat.creator),
        "disableanonymousjoin": "false", "gapDetectionEnabled": "True", "hasReplies": "False",
        "historydisclosed": "false", "isMigrated": "True", "productContext": "TEAMS",
        "tenantid": caller.tenant, "threadType": "chat",
    });
    if chat.unique_roster {
        properties["fixedRoster"] = json!(true);
        properties["uniquerosterthread"] = json!(true);
    }
    if let Some(topic) = &chat.topic {
        properties["topic"] = json!(topic);
    }
    json!({
        "copilotShareTokens": [], "eTag": format!("\"{}\"", chat.version), "encryptionType": "None",
        "fileReferences": {}, "id": chat.id, "members": members.iter().map(|m| member(app, m, true)).collect::<Vec<_>>(),
        "messages": format!("{}/v1/users/ME/conversations/{}/messages", base(app), chat.id),
        "properties": properties, "rosterVersion": chat.created_at,
        "tenantId": caller.tenant, "type": "Thread", "version": chat.version,
    })
}

pub(super) fn conversation(app: &App, caller: &Caller, chat: &Chat) -> Value {
    let mut properties = json!({
        "threadType": "chat", "productThreadType": if chat.unique_roster { "OneToOneChat" } else { "Chat" },
        "createdat": chat.created_at.to_string(), "isCreator": chat.creator == caller.id,
        "gapDetectionEnabled": "True", "lastjoinat": chat.created_at.to_string(),
        "originalThreadId": chat.id, "rosterVersion": chat.created_at, "version": chat.version.to_string(),
    });
    if chat.unique_roster {
        properties["uniquerosterthread"] = json!("True");
    }
    if let Some(topic) = &chat.topic {
        properties["topic"] = json!(topic);
    }
    json!({
        "id": chat.id, "type": "Conversation", "version": chat.version,
        "targetLink": format!("{}/v1/threads/{}", base(app), chat.id),
        "messages": format!("{}/v1/users/ME/conversations/{}/messages", base(app), chat.id),
        "lastUpdatedMessageId": 0, "lastUpdatedMessageVersion": 0, "lastMessage": {},
        "memberProperties": {"role": "Admin", "interest": "Interested", "isReader": false,
            "memberExpirationTime": 0, "relationshipState": {"inQuarantine": false}},
        "properties": {"addedBy": format!("8:orgid:{}", chat.creator), "addedByTenantId": caller.tenant,
            "isemptyconversation": "True"},
        "threadProperties": properties,
    })
}
