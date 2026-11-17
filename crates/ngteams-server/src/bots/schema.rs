diesel::table! {
    bot_deliveries (id) {
        id -> Text,
        bot_id -> Text,
        conversation_id -> Text,
        seq -> BigInt,
        activity -> Text,
        state -> Text,
        attempts -> Integer,
        max_attempts -> Integer,
        due_at -> BigInt,
        locked_until -> BigInt,
        locked_by -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    bot_health (bot_id) {
        bot_id -> Text,
        failures -> Integer,
        paused_until -> BigInt,
        last_error -> Nullable<Text>,
        last_success -> Nullable<BigInt>,
        updated_at -> BigInt,
    }
}

diesel::allow_tables_to_appear_in_same_query!(bot_deliveries, bot_health);
