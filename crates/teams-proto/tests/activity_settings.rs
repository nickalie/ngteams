//! Synthetic account app preferences and catalogue definitions; no tenant data.
use serde_json::{Value, json};
use teams_proto::{
    AppDefinition, Method, Read, Request, Service, SetUserGraphSettings, UserGraphSettings, UserGraphSettingsRead,
};

#[test]
fn activity_settings_preserve_other_apps_and_unrelated_fields() {
    let original = json!({
        "other-app": {"toastSettings": {"systemDefault": true}},
        "app": {"emailSettings": {"digest": true}, "future": [1, 2],
            "activityGenerationSettings": {"retainedType": true}, "toastSettings": {"retainedType": true}}
    });
    let body = json!({"userGraphSettings": original.to_string(), "otherProperty": "untouched"});
    let mut setting = UserGraphSettings::read_answer(body.to_string().as_bytes()).unwrap();
    setting
        .turn_off("app", &["customNotification".into(), "messageNotify".into()])
        .unwrap();
    let merged: Value = serde_json::from_str(&setting.json_text()).unwrap();
    assert_eq!(merged["other-app"], original["other-app"]);
    assert_eq!(merged["app"]["emailSettings"], original["app"]["emailSettings"]);
    assert_eq!(merged["app"]["future"], json!([1, 2]));
    for key in ["toastSettings", "activityGenerationSettings"] {
        assert_eq!(
            merged["app"][key],
            json!({"retainedType": true, "customNotification": false,
            "messageNotify": false, "systemDefault": false})
        );
    }
    let write = SetUserGraphSettings(setting);
    assert_eq!(write.method(), Method::Put);
    assert_eq!(write.service(), Service::ChatService);
    assert_eq!(write.body().unwrap()["userGraphSettings"], merged.to_string());
    assert_eq!(write.endpoint().segments(), ["v1", "users", "ME", "properties"]);
    assert!(write.is_write());
    assert_eq!(UserGraphSettingsRead.service(), Service::ChatService);
    assert_eq!(
        UserGraphSettingsRead.endpoint().segments(),
        ["v1", "users", "ME", "properties"]
    );
}

#[test]
fn activity_settings_absent_is_empty_but_unknown_is_not_overwritten() {
    for body in [
        "[]",
        "null",
        "broken",
        r#"{"userGraphSettings":null}"#,
        r#"{"userGraphSettings":{}}"#,
        r#"{"userGraphSettings":"[]"}"#,
        r#"{"userGraphSettings":"broken"}"#,
    ] {
        assert!(UserGraphSettings::read_answer(body.as_bytes()).is_err(), "{body}");
    }
    let mut settings = UserGraphSettings::read_answer(b"{}").unwrap();
    settings.turn_off("app", &[]).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&settings.json_text()).unwrap(),
        json!({"app": {
            "activityGenerationSettings": {"systemDefault": false}, "toastSettings": {"systemDefault": false}, "emailSettings": {}
        }})
    );
    for original in [json!({"app": []}), json!({"app": {"toastSettings": []}})] {
        let mut settings = UserGraphSettings::read_answer(
            json!({"userGraphSettings": original.to_string()})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(settings.turn_off("app", &["type".into()]).is_err());
        assert_eq!(serde_json::from_str::<Value>(&settings.json_text()).unwrap(), original);
    }
}

#[test]
fn activity_definitions_expose_names_icons_and_the_full_declared_type_set() {
    let definition = AppDefinition::new(
        json!({"id": "app", "name": "Made-up Updates", "largeImageUrl": "https://res.cdn.office.net/made-up.png",
        "activities": {"activityTypes": [{"type": "customNotification"}, {"type": "messageNotify"}]}})
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap();
    assert_eq!(definition.name(), Some("Made-up Updates"));
    assert_eq!(
        definition.large_image_url(),
        Some("https://res.cdn.office.net/made-up.png")
    );
    assert_eq!(
        definition.activity_types(),
        Some(vec!["customNotification".into(), "messageNotify".into()])
    );
    for activities in [
        json!({}),
        json!({"activityTypes": null}),
        json!({"activityTypes": [{"type": 2}]}),
        json!({"activityTypes": [{"type": ""}]}),
    ] {
        let definition = AppDefinition::new(
            json!({"id": "app", "activities": activities})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(definition.activity_types(), None);
    }
}

#[test]
fn activity_settings_undo_restores_only_the_changed_app_including_its_absence() {
    let mut original = UserGraphSettings::read_answer(b"{}").unwrap();
    let absent = original.app_preference("app").unwrap();
    original.turn_off("app", &["customNotification".into()]).unwrap();
    let disabled = original.app_preference("app").unwrap();
    original.turn_off("other-app", &[]).unwrap();
    original.restore_app("app", &absent);
    let value: Value = serde_json::from_str(&original.json_text()).unwrap();
    assert!(value.get("app").is_none());
    assert!(value.get("other-app").is_some());
    original.restore_app("app", &disabled);
    let value: Value = serde_json::from_str(&original.json_text()).unwrap();
    assert_eq!(value["app"]["toastSettings"]["customNotification"], false);
}

#[test]
fn activity_settings_only_the_known_no_properties_error_is_empty() {
    assert!(UserGraphSettingsRead::is_absent_answer(
        br#"{"errorCode":404,"standardizedError":{"errorCode":404,"errorSubCode":1}}"#
    ));
    for body in [
        "{}",
        "[]",
        "broken",
        r#"{"errorCode":404,"standardizedError":{"errorCode":404,"errorSubCode":2}}"#,
        r#"{"errorCode":404}"#,
        r#"{"errorCode":401,"standardizedError":{"errorCode":404,"errorSubCode":1}}"#,
    ] {
        assert!(!UserGraphSettingsRead::is_absent_answer(body.as_bytes()));
    }
}
