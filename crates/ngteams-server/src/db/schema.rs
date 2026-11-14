//! The tables, the same on both backends (`migrations/postgres`, `migrations/sqlite`).

diesel::table! {
    server_meta (name) {
        name -> Text,
        value -> Text,
    }
}

diesel::table! {
    users (id) {
        id -> Text,
        upn -> Text,
        display_name -> Text,
        password_hash -> Text,
        disabled -> Bool,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    refresh_tokens (id) {
        id -> Text,
        family_id -> Text,
        user_id -> Text,
        token_hash -> Text,
        client_id -> Text,
        created_at -> BigInt,
        expires_at -> BigInt,
        used_at -> Nullable<BigInt>,
        revoked_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    signing_keys (kid) {
        kid -> Text,
        private_der -> Binary,
        public_der -> Binary,
        created_at -> BigInt,
        retired_at -> Nullable<BigInt>,
    }
}

diesel::joinable!(refresh_tokens -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(server_meta, users, refresh_tokens, signing_keys);
