diesel::table! {
    bot_connections (bot_id, name) {
        bot_id -> Text,
        name -> Text,
        authorize_url -> Text,
        token_url -> Text,
        client_id -> Text,
        client_secret -> Binary,
        scopes -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    bot_sign_ins (id) {
        id -> Text,
        bot_id -> Text,
        connection -> Text,
        user_id -> Text,
        verifier -> Binary,
        code_hash -> Nullable<Text>,
        token -> Nullable<Binary>,
        attempts -> Integer,
        expires_at -> BigInt,
        created_at -> BigInt,
    }
}

diesel::table! {
    bot_user_tokens (bot_id, connection, user_id) {
        bot_id -> Text,
        connection -> Text,
        user_id -> Text,
        token -> Binary,
        updated_at -> BigInt,
    }
}

diesel::allow_tables_to_appear_in_same_query!(bot_connections, bot_sign_ins, bot_user_tokens);
