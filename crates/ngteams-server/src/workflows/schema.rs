use crate::db::schema::users;

diesel::table! {
    workflow_connections (name) {
        name -> Text,
        owner_id -> Text,
        api -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    workflows (id) {
        id -> Text,
        creator_id -> Text,
        display_name -> Text,
        enabled -> Bool,
        properties -> Text,
        target_thread -> Text,
        secret -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    workflow_owners (workflow_id, user_id) {
        workflow_id -> Text,
        user_id -> Text,
    }
}

diesel::table! {
    workflow_runs (workflow_id, id) {
        workflow_id -> Text,
        id -> Text,
        started_at -> BigInt,
        ended_at -> BigInt,
        status -> Text,
        error -> Nullable<Text>,
    }
}

diesel::joinable!(workflow_owners -> workflows (workflow_id));
diesel::joinable!(workflow_owners -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(workflows, workflow_owners, users);
