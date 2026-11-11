//! Merge each service participant independently before choosing an endpoint. A body can contain a delta or
//! departure for the followed MRI beside a full read of the other: neither may hide the other entry.
use std::collections::BTreeMap;

use serde_json::Value;

use super::{CAPTION_SERVICE_MRIS, CaptionCommandLink, CaptionService};

#[derive(Clone, Debug, Default)]
struct Participant {
    version: Option<u64>,
    active: bool,
    endpoints: BTreeMap<String, Value>,
}

/// The two service participants, each with its own version, activity and accumulated endpoint metadata.
#[derive(Clone, Debug, Default)]
pub struct CaptionRoster {
    participants: BTreeMap<&'static str, Participant>,
    service: Option<CaptionService>,
}

/// The newest active endpoint, including recording-only endpoints without a caption command link.
#[derive(Clone, Copy, Debug)]
pub struct FollowedService<'a> {
    pub mri: &'static str,
    pub endpoint_id: &'a str,
    pub metadata: &'a Value,
}

impl FollowedService<'_> {
    fn captions(self) -> Option<CaptionService> {
        Some(CaptionService {
            mri: self.mri,
            endpoint_id: self.endpoint_id.to_owned(),
            command: CaptionCommandLink::parse(self.metadata.get("commandUrl")?.as_str()?)?,
            spoken_language: self
                .metadata
                .get("spokenLanguage")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            state: self
                .metadata
                .pointer("/processingModes/closedCaptions/state")?
                .as_str()?
                .into(),
        })
    }
}

impl CaptionRoster {
    /// Applies every named service entry under that MRI's own monotonically increasing version. Omitted fields
    /// are deltas, including omitted endpoints and join times; an explicit departure removes that MRI's endpoints.
    pub fn update(&mut self, roster: &Value) {
        for mri in CAPTION_SERVICE_MRIS {
            let Some(entry) = roster.get("participants").and_then(|entries| entries.get(mri)) else {
                continue;
            };
            self.participants.entry(mri).or_default().update(entry);
        }
        self.service = self.followed().and_then(FollowedService::captions);
    }

    /// Pure selection across all accumulated active endpoints: newest join first, invited MRI first on a tie.
    /// Missing or unreadable join times sort before readable ones. No active endpoint means no followed service.
    #[must_use]
    pub fn followed(&self) -> Option<FollowedService<'_>> {
        CAPTION_SERVICE_MRIS
            .into_iter()
            .flat_map(|mri| {
                self.participants
                    .get(mri)
                    .filter(|entry| entry.active)
                    .into_iter()
                    .flat_map(move |entry| {
                        entry.endpoints.iter().filter_map(move |(id, endpoint)| {
                            let inactive = endpoint
                                .get("state")
                                .and_then(Value::as_str)
                                .is_some_and(|state| state != "active");
                            (!inactive).then_some(FollowedService {
                                mri,
                                endpoint_id: id,
                                metadata: endpoint.get("endpointMetadata")?,
                            })
                        })
                    })
            })
            .reduce(|kept, next| {
                if join_order(next.metadata) > join_order(kept.metadata) {
                    next
                } else {
                    kept
                }
            })
    }

    /// The followed endpoint's caption state and validated link, absent unless that endpoint supplies both.
    #[must_use]
    pub fn service(&self) -> Option<&CaptionService> {
        self.service.as_ref()
    }
}

impl Participant {
    fn update(&mut self, entry: &Value) {
        if let Some(version) = entry.get("version").and_then(Value::as_u64) {
            if self.version.is_some_and(|old| version <= old) {
                return;
            }
            self.version = Some(version);
        }
        if let Some(state) = entry.get("state").and_then(Value::as_str) {
            self.active = state == "active";
            if !self.active {
                self.endpoints.clear();
                return;
            }
        }
        if let Some(endpoints) = entry.get("endpoints").and_then(Value::as_object) {
            for (id, endpoint) in endpoints {
                if endpoint.is_null() {
                    self.endpoints.remove(id);
                } else {
                    merge(self.endpoints.entry(id.clone()).or_insert(Value::Null), endpoint);
                }
            }
        }
    }
}

fn merge(previous: &mut Value, delta: &Value) {
    if let (Some(previous), Some(delta)) = (previous.as_object_mut(), delta.as_object()) {
        for (key, value) in delta {
            let field = previous.entry(key.clone()).or_insert(Value::Null);
            // A supplied processingModes map is the endpoint's mode snapshot (including stopped modes).
            // Omitting the whole map is a metadata delta and preserves the snapshot.
            if key == "processingModes" {
                field.clone_from(value);
            } else {
                merge(field, value);
            }
        }
    } else {
        previous.clone_from(delta);
    }
}

fn join_order(metadata: &Value) -> Option<(String, u32)> {
    let joined = metadata.get("joinTime")?.as_str()?.strip_suffix('Z')?;
    let (seconds, fraction) = joined.split_once('.').unwrap_or((joined, ""));
    if fraction.len() > 9 || !fraction.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    Some((seconds.to_owned(), format!("{fraction:0<9}").parse().ok()?))
}

#[cfg(test)]
mod tests;
