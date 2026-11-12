//! A person's profile card, read when a view opens one (`docs/protocol/loki.md`), as Teams' card reads it: Loki's
//! person, schedule, out-of-office, organization and working-with reads ([`teams_proto::loki`]), one UPS presence read
//! of them ([`PresenceRead`]: availability, when they were last active, out of office) and the middle tier's short
//! profile ([`ShortProfileLookup`]: job title, office, and their names and addresses when Loki gives none), all at
//! once. Every one is a read that reaches nobody; nothing is stored.
//!
//! Loki's configuration is read once per engine, the first time a card is asked for (again after one that failed), and
//! the regional base it names handed to the reads' transport ([`HttpTransport::set_loki_region`]). Without it every
//! Loki read is refused and the card is made of the rest.
//!
//! A section whose read failed is `None` (or, for the names and addresses, filled from the short profile); every read
//! failing, or an MRI that isn't `8:orgid:<object id>`, is [`EngineError::ProfileUnread`].

use std::fmt;
use std::sync::Arc;

use teams_proto::{
    Client, Credentials, HttpError, HttpTransport, LokiConfiguration, LokiOrganization, LokiOutOfOffice, LokiPerson,
    LokiSchedule, LokiWorkingWith, Mri, OutOfOfficeState, PersonDetails, PersonaRef, PresenceRead, Read, Schedule,
    ShortProfile, ShortProfileLookup, UserPresence, object_id,
};
use tokio::sync::OnceCell;

use crate::engine::{EngineError, Responder};
use crate::presence::Availability;

/// Someone a card names: a manager, a direct report, someone the person works with.
#[derive(Clone, PartialEq, Eq)]
pub struct CardPerson {
    /// `8:orgid:<object id>`.
    pub mri: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

impl From<PersonaRef> for CardPerson {
    fn from(person: PersonaRef) -> Self {
        Self {
            mri: person.mri(),
            name: person.name,
            email: person.email.or(person.user_principal_name),
        }
    }
}

/// `Debug` names them by their MRI only.
impl fmt::Debug for CardPerson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CardPerson")
            .field("mri", &self.mri)
            .finish_non_exhaustive()
    }
}

/// A person's managers and direct reports (Loki's organization read; its entries' shape unverified).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardOrganization {
    /// Nearest first, as given.
    pub managers: Vec<CardPerson>,
    pub directs: Vec<CardPerson>,
}

/// A person's presence as UPS gave it for the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardPresence {
    pub availability: Availability,
    /// When they were last active, ms since the epoch; `None` when UPS says never (Teams then shows "Offline", not
    /// "Last seen").
    pub last_active_ms: Option<i64>,
    /// Whether their calendar says they are out of office.
    pub out_of_office: Option<bool>,
}

/// What a person's profile card shows ([`crate::EngineRequests::profile_card`]). Each section is `None` when its
/// read failed (or gave nothing); the names and addresses come from Loki's person, else the short profile. `Debug`
/// leaves names and addresses out.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProfileCard {
    /// The person's MRI, as asked.
    pub mri: String,
    /// Their display name.
    pub name: Option<String>,
    pub given_name: Option<String>,
    /// Their mail addresses, the main one first.
    pub emails: Vec<String>,
    /// Their chat (IM) addresses, as Teams shows them beside "Chat".
    pub im_addresses: Vec<String>,
    pub user_principal_name: Option<String>,
    /// `Member`, `Guest`, … as the directory gives it.
    pub user_type: Option<String>,
    /// Their job title and office, as the short profile gives them.
    pub job_title: Option<String>,
    pub office: Option<String>,
    /// Their presence (UPS's read).
    pub presence: Option<CardPresence>,
    /// Whether they are out of office: UPS's calendar flag, else `false` when Loki says their automatic replies are
    /// off; `None` when neither says.
    pub out_of_office: Option<bool>,
    /// Their free/busy status and working hours (Loki's schedule).
    pub schedule: Option<Schedule>,
    /// Their managers and direct reports (Loki's organization).
    pub organization: Option<CardOrganization>,
    /// The people they work with (Loki's working-with), at most 12.
    pub working_with: Option<Vec<CardPerson>>,
}

impl fmt::Debug for ProfileCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileCard")
            .field("mri", &self.mri)
            .field("presence", &self.presence)
            .field("out_of_office", &self.out_of_office)
            .field("schedule", &self.schedule)
            .field("organization", &self.organization)
            .field("working_with", &self.working_with)
            .finish_non_exhaustive()
    }
}

/// The profile card reads of one account.
pub(crate) struct ProfileCards<C> {
    client: Arc<Client<HttpTransport<C>>>,
    /// Loki's configuration, read once (and its regional base handed to the transport).
    configured: Arc<OnceCell<()>>,
}

impl<C: Credentials + 'static> ProfileCards<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self {
            client,
            configured: Arc::new(OnceCell::new()),
        }
    }

    /// `mri`'s card, read now.
    pub(crate) fn card(&self, mri: String, reply: Responder<ProfileCard>) {
        let client = Arc::clone(&self.client);
        let configured = Arc::clone(&self.configured);
        tokio::task::spawn_local(async move {
            let _ = reply.send(read_card(&client, &configured, mri).await);
        });
    }
}

/// The reads' answers, each `None` when it failed.
struct Reads {
    person: Option<PersonDetails>,
    schedule: Option<Schedule>,
    out_of_office: Option<OutOfOfficeState>,
    organization: Option<CardOrganization>,
    working_with: Option<Vec<CardPerson>>,
    presence: Option<UserPresence>,
    profile: Option<ShortProfile>,
}

impl Reads {
    fn none_read(&self) -> bool {
        self.person.is_none()
            && self.schedule.is_none()
            && self.out_of_office.is_none()
            && self.organization.is_none()
            && self.working_with.is_none()
            && self.presence.is_none()
            && self.profile.is_none()
    }
}

async fn read_card<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    configured: &OnceCell<()>,
    mri: String,
) -> Result<ProfileCard, EngineError> {
    let (Some(person), Some(organization), Some(working), Some(schedule), Some(oof)) = (
        LokiPerson::of(&mri),
        LokiOrganization::of(&mri),
        LokiWorkingWith::of(&mri),
        LokiSchedule::of(&mri),
        LokiOutOfOffice::of(&mri),
    ) else {
        log::info!("a profile card was asked for an account that isn't a directory user's");
        return Err(EngineError::ProfileUnread);
    };
    configure(client, configured).await;
    let read_person = |body: &[u8]| person.read_answer(body).ok();
    let read_schedule = |body: &[u8]| LokiSchedule::read_answer(body).ok();
    let read_oof = |body: &[u8]| LokiOutOfOffice::read_answer(body).ok();
    let (person, organization, working_with, schedule, out_of_office, presence, profile) = tokio::join!(
        loki(client, &person, "person", read_person),
        loki(client, &organization, "organization", organization_of),
        loki(client, &working, "working-with", working_with_of),
        loki(client, &schedule, "schedule", read_schedule),
        loki(client, &oof, "out-of-office", read_oof),
        presence(client, &mri),
        short_profile(client, &mri),
    );
    let reads = Reads {
        person,
        schedule,
        out_of_office,
        organization,
        working_with,
        presence,
        profile,
    };
    if reads.none_read() {
        return Err(EngineError::ProfileUnread);
    }
    Ok(card_of(mri, reads))
}

/// An organization answer's managers and direct reports.
fn organization_of(body: &[u8]) -> Option<CardOrganization> {
    let read = LokiOrganization::read_answer(body).ok()?;
    Some(CardOrganization {
        managers: read.managers.into_iter().map(CardPerson::from).collect(),
        directs: read.directs.into_iter().map(CardPerson::from).collect(),
    })
}

/// A working-with answer's people.
fn working_with_of(body: &[u8]) -> Option<Vec<CardPerson>> {
    let people = LokiWorkingWith::read_answer(body).ok()?;
    Some(people.into_iter().map(CardPerson::from).collect())
}

/// Reads Loki's configuration and hands its regional base to the transport, once per engine (again after a failure);
/// nothing when the transport wasn't given Loki or has its region already.
async fn configure<C: Credentials>(client: &Client<HttpTransport<C>>, configured: &OnceCell<()>) {
    let transport = client.transport();
    if !transport.has_loki() || transport.loki_region().is_some() {
        return;
    }
    let read = configured
        .get_or_try_init(|| async {
            let answer = client.read(&LokiConfiguration).await.map_err(|error| {
                log::info!("Loki's configuration couldn't be read: {error}");
            })?;
            let base = LokiConfiguration::read_answer(answer.body()).ok_or_else(|| {
                log::info!("Loki's configuration names no regional base the transport takes");
            })?;
            transport.set_loki_region(&base).map_err(|error| {
                log::info!("Loki's regional base wasn't taken: {error}");
            })
        })
        .await;
    if read.is_ok() {
        log::debug!("Loki's regional base is set");
    }
}

/// A Loki read's answer, read by `read_answer`; `None` when it failed (logged by `what`, never an id or a body).
async fn loki<C: Credentials, T>(
    client: &Client<HttpTransport<C>>,
    request: &impl Read,
    what: &str,
    read_answer: impl FnOnce(&[u8]) -> Option<T>,
) -> Option<T> {
    match client.read(request).await {
        Ok(answer) => {
            let read = read_answer(answer.body());
            if read.is_none() {
                log::info!("a profile card's {what} answer isn't one the app reads");
            }
            read
        }
        Err(error) => {
            log_failed(what, &error);
            None
        }
    }
}

fn log_failed(what: &str, error: &HttpError) {
    log::info!("a profile card's {what} couldn't be read: {error}");
}

/// `mri`'s presence as UPS reads it now.
async fn presence<C: Credentials>(client: &Client<HttpTransport<C>>, mri: &str) -> Option<UserPresence> {
    let request = PresenceRead::new([Mri::new(mri)])?;
    match client.post_read(request.clone()).await {
        Ok(answer) => request
            .read_answer(answer.body())
            .ok()?
            .into_iter()
            .find(|presence| presence.mri.as_str() == mri),
        Err(error) => {
            log_failed("presence", &error);
            None
        }
    }
}

/// `mri`'s short profile from the middle tier.
async fn short_profile<C: Credentials>(client: &Client<HttpTransport<C>>, mri: &str) -> Option<ShortProfile> {
    let lookup = ShortProfileLookup::new([Mri::new(mri)])?;
    match client.lookup(lookup.clone()).await {
        Ok(answer) => lookup
            .read_answer(answer.body())
            .ok()?
            .into_iter()
            .find(|profile| profile.mri.as_str() == mri),
        Err(error) => {
            log_failed("short profile", &error);
            None
        }
    }
}

/// The card the reads make.
fn card_of(mri: String, reads: Reads) -> ProfileCard {
    let person = reads.person.unwrap_or_default();
    let profile = reads.profile;
    let field = |of: fn(&ShortProfile) -> Option<&String>| profile.as_ref().and_then(of).cloned();
    let presence = reads.presence.and_then(|presence| {
        let read = CardPresence {
            availability: Availability::of(presence.availability.as_deref()),
            last_active_ms: presence.last_active_ms,
            out_of_office: presence.out_of_office,
        };
        // an item UPS didn't read (another status) gives nothing
        (read.availability != Availability::Unknown || presence.out_of_office.is_some()).then_some(read)
    });
    let replies_off = matches!(reads.out_of_office, Some(OutOfOfficeState::Disabled)).then_some(false);
    let out_of_office = presence
        .as_ref()
        .and_then(|presence| presence.out_of_office)
        .or(replies_off);
    ProfileCard {
        name: person
            .display_name
            .or_else(|| field(|profile| profile.display_name.as_ref())),
        given_name: person
            .given_name
            .or_else(|| field(|profile| profile.given_name.as_ref())),
        emails: or_one(person.emails, field(|profile| profile.email.as_ref())),
        im_addresses: person.im_addresses,
        user_principal_name: person
            .user_principal_name
            .or_else(|| field(|profile| profile.user_principal_name.as_ref())),
        user_type: person.user_type.or_else(|| field(|profile| profile.user_type.as_ref())),
        job_title: field(|profile| profile.job_title.as_ref()),
        office: field(|profile| profile.office_location.as_ref()),
        presence,
        out_of_office,
        schedule: reads.schedule,
        organization: reads.organization,
        working_with: reads.working_with,
        mri,
    }
}

/// `list`, or `fallback` alone when `list` is empty.
fn or_one(list: Vec<String>, fallback: Option<String>) -> Vec<String> {
    if list.is_empty() {
        fallback.into_iter().collect()
    } else {
        list
    }
}

/// Whether `mri` can have a card: `8:orgid:<object id>`.
#[must_use]
pub fn has_card(mri: &str) -> bool {
    object_id(mri).is_some()
}
