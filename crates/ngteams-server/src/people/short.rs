//! The middle tier's `users/fetchShortProfile`: names, addresses, title and office of the accounts a list of MRIs
//! names, or with `isMailAddress=true` a list of addresses (`docs/protocol/chat-service.md`, "Reads by POST"). A
//! disabled account still answers, so what it wrote keeps its author's name; a deleted one doesn't.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde_json::{Map, Value, json};

use super::repo::{self, Person};
use super::{Caller, unavailable};
use crate::app::App;
use crate::auth::extract::Spaces;

/// The names one lookup takes.
const MAX_NAMES: usize = 500;

/// A person's short profile; a field the directory doesn't have is left out, as Teams leaves it out.
fn short_profile(person: &Person, tenant: &str) -> Value {
    let (user, profile) = (&person.user, &person.profile);
    let mut fields = Map::new();
    let mut put = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            fields.insert(key.to_owned(), json!(value));
        }
    };
    put("mri", Some(&format!("8:orgid:{}", user.id)));
    put("objectId", Some(&user.id));
    put("displayName", Some(&user.display_name));
    put("givenName", profile.given_name.as_deref());
    put("surname", profile.surname.as_deref());
    put("email", Some(&user.upn));
    put("userPrincipalName", Some(&user.upn));
    put("jobTitle", profile.job_title.as_deref());
    put("department", profile.department.as_deref());
    put("officeLocation", profile.office_location.as_deref());
    put("companyName", profile.company_name.as_deref());
    put("mobile", profile.mobile_phone.as_deref());
    put("telephoneNumber", profile.business_phone.as_deref());
    put("userType", Some("Member"));
    put("tenantId", Some(tenant));
    put("tenantName", Some(""));
    put("type", Some("ADUser"));
    fields.insert("accountEnabled".to_owned(), json!(!user.disabled));
    Value::Object(fields)
}

pub(super) async fn short_profiles(
    State(app): State<Arc<App>>,
    caller: Caller<Spaces>,
    Query(query): Query<HashMap<String, String>>,
    names: Result<Json<Vec<String>>, JsonRejection>,
) -> Result<Json<Value>, StatusCode> {
    let Json(names) = names.map_err(|_| StatusCode::BAD_REQUEST)?;
    if names.len() > MAX_NAMES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let found = if query.get("isMailAddress").is_some_and(|value| value == "true") {
        repo::people_by_address(&app.db, &names).await
    } else {
        let ids: Vec<String> = names
            .iter()
            .filter_map(|name| name.strip_prefix("8:orgid:"))
            .map(str::to_owned)
            .collect();
        repo::people(&app.db, &ids).await
    };
    let found = found.map_err(|error| unavailable(&error))?;
    let profiles: Vec<Value> = found
        .iter()
        .map(|person| short_profile(person, &caller.tenant))
        .collect();
    Ok(Json(json!({
        "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": profiles,
    })))
}
