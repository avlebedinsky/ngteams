use crate::chatsvc::schema::chats;
use crate::db::schema::users;

diesel::table! {
    calendar_events (id) {
        id -> Text,
        owner_id -> Text,
        team_id -> Nullable<Text>,
        channel_id -> Nullable<Text>,
        post_id -> Nullable<BigInt>,
        series_id -> Nullable<Text>,
        original_start -> Nullable<BigInt>,
        start_ms -> BigInt,
        end_ms -> BigInt,
        until_ms -> BigInt,
        recurring -> Bool,
        changed -> Bool,
        cancelled -> Bool,
        meeting_chat -> Nullable<Text>,
        body -> Text,
        version -> BigInt,
    }
}

diesel::table! {
    calendar_attendees (event_id, user_id) {
        event_id -> Text,
        user_id -> Text,
        position -> Integer,
        optional -> Bool,
        response -> Text,
        responded_at -> Nullable<BigInt>,
        removed -> Bool,
    }
}

diesel::table! {
    meetings (chat_id) {
        chat_id -> Text,
        organizer_id -> Text,
        code -> Text,
        passcode -> Text,
        subject -> Nullable<Text>,
        kind -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    meeting_options (chat_id) {
        chat_id -> Text,
        body -> Text,
    }
}

diesel::joinable!(calendar_attendees -> calendar_events (event_id));
diesel::allow_tables_to_appear_in_same_query!(calendar_events, calendar_attendees, meetings, meeting_options);
diesel::allow_tables_to_appear_in_same_query!(calendar_attendees, users);
diesel::allow_tables_to_appear_in_same_query!(meetings, chats);
