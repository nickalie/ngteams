diesel::table! {
    media_objects (id) {
        id -> Text,
        owner_id -> Text,
        kind -> Text,
        filename -> Text,
        content_type -> Nullable<Text>,
        blob -> Nullable<Text>,
        size -> BigInt,
        created_at -> BigInt,
    }
}

diesel::table! {
    media_object_readers (object_id, chat_id) {
        object_id -> Text,
        chat_id -> Text,
    }
}

diesel::table! {
    drive_items (id) {
        id -> Text,
        owner_id -> Text,
        name -> Text,
        size -> BigInt,
        created_at -> BigInt,
    }
}

diesel::table! {
    drive_grants (item_id, user_id) {
        item_id -> Text,
        user_id -> Text,
    }
}

diesel::table! {
    media_usage (user_id) {
        user_id -> Text,
        bytes -> BigInt,
    }
}

diesel::table! {
    channel_files (id) {
        id -> Text,
        team_id -> Text,
        channel_id -> Text,
        folder -> Text,
        name -> Text,
        owner_id -> Text,
        stage -> Text,
        upload_id -> Nullable<Text>,
        size -> BigInt,
        created_at -> BigInt,
    }
}

diesel::allow_tables_to_appear_in_same_query!(drive_items, drive_grants);
