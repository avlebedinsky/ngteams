//! CSA's teams read (M2 Task 16): `GET /api/csa/<region>/api/v3/teams/users/me` with the web client's query exactly,
//! answered with `csa/teams.json`'s body (the same teams for every account: the mock's one team, [`crate::CHANNEL`],
//! with its other channel, [`crate::TEAM_CHANNEL`]). It takes the account's token alone, as the probe sent it; a
//! request without a token the mock handed out is a 401, the read with any other query a 404 (the service's answers
//! to those weren't captured).

use percent_encoding::percent_decode_str;
use wiremock::{Request, ResponseTemplate};

use crate::REGION;
use crate::chatsvc::{json, status};
use crate::world::State;

/// The read's query, in order (`teams_proto::CSA_TEAMS_QUERY`, which the mock doesn't depend on).
const QUERY: [(&str, &str); 5] = [
    ("isPrefetch", "false"),
    ("enableMembershipSummary", "true"),
    ("supportsAdditionalSystemGeneratedFolders", "true"),
    ("supportsSliceItems", "true"),
    ("enableEngageCommunities", "false"),
];

/// The answer to `request` when it is under the CSA base; `None` for any other path.
pub(crate) fn teams(state: &State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let rest = match segments.as_slice() {
        [api, csa, region, rest @ ..] if api == "api" && csa == "csa" && region == REGION => rest,
        _ => return None,
    };
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    if token.and_then(|token| state.account_by_token(token)).is_none() {
        return Some(status(401));
    }
    let query: Vec<(String, String)> = request.url.query_pairs().into_owned().collect();
    let the_read = request.method.as_str() == "GET"
        && rest == ["api", "v3", "teams", "users", "me"]
        && query
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .eq(QUERY);
    Some(if the_read {
        json(200, &state.fixtures.teams)
    } else {
        status(404)
    })
}
