diesel::table! {
    chat_clock (id) {
        id -> Integer,
        version -> BigInt,
    }
}

diesel::table! {
    chats (id) {
        id -> Text,
        creator -> Text,
        topic -> Nullable<Text>,
        unique_roster -> Bool,
        created_at -> BigInt,
        version -> BigInt,
    }
}

diesel::table! {
    chat_members (chat_id, user_id) {
        chat_id -> Text,
        user_id -> Text,
        position -> Integer,
        joined_at -> BigInt,
    }
}

diesel::joinable!(chat_members -> chats (chat_id));
diesel::allow_tables_to_appear_in_same_query!(chats, chat_members);
