diesel::table! {
    outgoing_webhooks (id) {
        id -> Text,
        team_id -> Text,
        name -> Text,
        name_key -> Text,
        description -> Text,
        callback_url -> Text,
        security_key -> Text,
        picture_etag -> Nullable<Text>,
        created_by -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    outgoing_webhook_deliveries (id) {
        id -> Text,
        webhook_id -> Text,
        chat_id -> Text,
        root_id -> BigInt,
        activity -> Text,
        state -> Text,
        locked_by -> Nullable<Text>,
        locked_until -> BigInt,
        last_error -> Nullable<Text>,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::joinable!(outgoing_webhook_deliveries -> outgoing_webhooks (webhook_id));
diesel::allow_tables_to_appear_in_same_query!(outgoing_webhooks, outgoing_webhook_deliveries);
