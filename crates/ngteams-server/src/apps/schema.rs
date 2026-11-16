diesel::table! {
    apps (id) {
        id -> Text,
        version -> Text,
        name -> Text,
        full_name -> Text,
        short_description -> Text,
        full_description -> Text,
        developer -> Text,
        accent_color -> Text,
        manifest -> Text,
        source -> Text,
        icons -> Bool,
        everyone -> Bool,
        enabled -> Bool,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    app_users (app_id, user_id) {
        app_id -> Text,
        user_id -> Text,
    }
}

diesel::table! {
    bots (id) {
        id -> Text,
        app_id -> Text,
        endpoint -> Nullable<Text>,
        secret_hash -> Text,
        auth_mode -> Text,
        scopes -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    app_installs (app_id, scope, target) {
        app_id -> Text,
        scope -> Text,
        target -> Text,
        installed_by -> Nullable<Text>,
        installed_at -> BigInt,
    }
}

diesel::table! {
    bot_signing_keys (kid) {
        kid -> Text,
        private_der -> Binary,
        created_at -> BigInt,
        retired_at -> Nullable<BigInt>,
    }
}

diesel::joinable!(bots -> apps (app_id));
diesel::joinable!(app_users -> apps (app_id));
diesel::allow_tables_to_appear_in_same_query!(apps, bots, app_users, app_installs);
