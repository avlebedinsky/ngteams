use crate::chatsvc::schema::{chat_members, chats};
use crate::db::schema::users;

diesel::table! {
    teams (id) {
        id -> Text,
        group_id -> Text,
        name -> Text,
        description -> Text,
        public -> Bool,
        archived -> Bool,
        join_code -> Nullable<Text>,
        settings -> Text,
        tag_settings -> Text,
        picture_etag -> Text,
        created_at -> BigInt,
        version -> BigInt,
    }
}

diesel::table! {
    team_members (team_id, user_id) {
        team_id -> Text,
        user_id -> Text,
        owner -> Bool,
        hidden -> Bool,
        joined_at -> BigInt,
    }
}

diesel::table! {
    channels (id) {
        id -> Text,
        team_id -> Text,
        group_id -> Nullable<Text>,
        name -> Text,
        description -> Text,
        private -> Bool,
        layout -> Text,
        archived -> Bool,
        shown_by_default -> Bool,
        settings -> Nullable<Text>,
        created_at -> BigInt,
        version -> BigInt,
    }
}

diesel::table! {
    channel_roles (channel_id, user_id) {
        channel_id -> Text,
        user_id -> Text,
        owner -> Bool,
        moderator -> Bool,
    }
}

diesel::table! {
    team_join_requests (team_id, user_id) {
        team_id -> Text,
        user_id -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    team_tags (id) {
        id -> Text,
        team_id -> Text,
        name -> Text,
        description -> Text,
        members -> Text,
        version -> BigInt,
    }
}

diesel::table! {
    conversation_folders (user_id) {
        user_id -> Text,
        version -> BigInt,
        body -> Text,
    }
}

diesel::joinable!(team_members -> teams (team_id));
diesel::joinable!(channels -> teams (team_id));
diesel::allow_tables_to_appear_in_same_query!(teams, team_members, channels, channel_roles, team_join_requests);
diesel::allow_tables_to_appear_in_same_query!(channels, chats);
diesel::allow_tables_to_appear_in_same_query!(channels, chat_members);
diesel::allow_tables_to_appear_in_same_query!(team_members, users);
diesel::allow_tables_to_appear_in_same_query!(team_members, chat_members);
