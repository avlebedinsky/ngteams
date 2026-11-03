//! Teams internal API requests and the native request policy. Every contacting write takes a `WritePermit` that only
//! `teams-fence` can issue.
//!
//! A request comes in one of three classes, and a [`Transport`] takes nothing else:
//! - a [`Read`]: always sent as a `GET`, with no body (the chat service's, Microsoft Graph's reads of a person's name
//!   and picture, [`GraphUser`] and [`GraphPicture`], and a shared file's thumbnails, [`FileThumbnails`], CSA's read
//!   of the account's teams, [`CsaTeams`], and a picture pasted into a message, [`MessagePicture`], from AMS, a link
//!   preview's picture, [`LinkPreviewPicture`], from the async gateway's URL preview service, and a GIF
//!   a message shows, [`GiphyPicture`], from Giphy's media host with no token; beside them a file thumbnail's picture
//!   at the link Graph handed out, [`HttpTransport::file_thumbnail`], with no token, and a shared file's item and its
//!   bytes, part by part, [`FileDownload`] and [`HttpTransport::file_range`], to play a video inline);
//! - a plain [`Request`]: any method and body, the trait open to any crate. The transport builds its URL from its
//!   [`Service`]'s base and asks [`teams_fence::contacting`], the lab rule's one classifier: sent when it reaches nobody
//!   (the messaging stack's [`EndpointRegistration`], the middle tier's profile lookup [`ShortProfileLookup`], UPS's
//!   presence read [`PresenceRead`], whoever they name), logged to `fence.log` as `allowed` before it goes; refused with
//!   a typed error, before anything is sent, when it is a write or a request on a contacting service the classifier
//!   can't read;
//! - a write: a request that reaches other people implements [`Mutating`] and names its one target. [`Client::write`]
//!   takes the request together with a [`WritePermit`](teams_fence::WritePermit), so a mutating call without a permit
//!   doesn't compile, and a permit can't be made outside `teams-fence`. At runtime [`Authorized::new`] checks that the
//!   permit was issued for the request's own target before anything reaches the [`Transport`], which sends the
//!   request's sealed wire form and returns the service's answer. The chat writes (a message post, an edit, a delete, a
//!   reaction and taking it back, a read receipt, typing, a new chat) are such requests, each with its target and the
//!   wire form the web client sent; a message's text is a [`chat::MessageText`], never raw HTML. So is the account's own
//!   availability (UPS's presence endpoint `PUT`, [`SetEndpointAvailability`], and the manual status,
//!   [`ForceAvailability`]), on the account itself, naming only the endpoint a registration of this run registered for
//!   that account ([`RegisteredEndpoint`], which only [`register`] gives, over the native transport:
//!   [`EndpointRegistrar`], recorded for its fence's own account). And so are a call's requests (M3): the call start
//!   ([`StartCall`]), the join, the callee's ring-time sends, answer and decline, and every captured operation on a
//!   call, each built whole when it is made and classified by [`teams_fence::contacting`], its target the one the
//!   classifier gives, sent to [`Service::Calling`] with the account's skypetoken, the link exactly as the call's
//!   services handed it out ([`CallLink`]), the fence's call registry fed around the send.
//!
//! The personal build's sealed lists stay beside them: [`OwnState`] and [`PostRead`] (each entry also a plain request),
//! sent by [`HttpTransport::own_state`] and [`HttpTransport::post_read`] (the latter only inside an
//! [`AuthorizedPostRead`] with the [`PostReadPermit`](teams_fence::PostReadPermit) `Fence::check_post_read` gave for its
//! exact MRIs, in its entry's captured shape, once).
//!
//! On the push channel's socket a [`FrameSink`] sends only an [`OwnFrame`], a typed frame this crate builds (today the
//! Trouter session's own: authentication, keep-alive, the message-loss acknowledgement, the disconnect), never raw
//! text, and none of them is written to `fence.log` (a frame isn't a contacting write). The only answer to a delivery
//! is a typed call delivery response ([`AcceptanceAcknowledgement`], [`InvitationReceipt`]), which only
//! [`FrameSink::respond`] sends (or [`FrameSink::send_permitted`], after a [`ResponseDecider`] the socket reads on
//! beside), once the fence permitted exactly its JSON for the sink's connection.
//!
//! [`HttpTransport`] is the live transport: one base per service, `https` only, no proxy, no redirects, each of the
//! account's tokens (from its `teams-auth` session) only to its own service's host. Only the `test-support` feature
//! (tests, never a lab binary) points it at a local mock (`HttpTransport::with_mock_origin`). It is also where the
//! fence's live membership read takes the native client's own credential from ([`teams_fence::CredentialSource`]):
//! the account's `ic3` token and the transport's chat base.

mod ams;
mod attachment;
mod calendar;
pub mod captions;
mod channel_meeting;
mod channel_own;
mod channel_read;
mod channel_sections;
pub use channel_own::ChannelOwn;
pub use channel_sections::{ChannelSection, ChannelSectionAction, ChannelSectionWrite, ChannelSections};
pub use write::{ChannelChange, ChannelData, ChannelRead, ChannelTarget, ChannelWrite};
pub mod chat;
mod client;
mod emoticon;
mod endpoint;
mod file_content;
mod frame;
mod giphy;
mod http;
pub mod lab_calls;
mod meeting;
mod own;
mod post_read;
mod read;
mod recurrence;
mod relay;
mod request;
mod schedule;
mod search;
mod team_apps;
mod team_read;
mod thumbnail;
pub use attachment::{DriveSite, DriveWrite, MAX_ATTACHMENT_SIZE, OwnDrive, UploadedFile, direct_file_grants};
mod urlp;
pub mod workflow;
mod write;

pub use ams::{MessagePicture, PastedFormat, PastedPicture};
pub use calendar::{
    Attendee, CALENDAR_VIEW_PATH, CalendarAnswer, CalendarAnswerError, CalendarEvent, CalendarView, ChannelEventRead,
    ChannelEventRef, EventKind, EventRead, EventTime, FloatingTime, FullEvent, MAX_CALENDAR_WINDOW_MS,
    Response as EventResponse, ShowAs, TimeZoneSettings, join_url_chat, join_url_meeting,
};
pub use channel_meeting::{CANCELLED_MEETING_TYPE, SCHEDULED_MEETING_TYPE, ScheduledMeeting};
pub use channel_read::{ChannelCreationStatus, TeamPeopleSearch, TeamPerson};
pub use client::{Client, PostReadError, Response, Transport, WriteError};
pub use emoticon::{MAX_ANIMATION, ReactionAnimation};
pub use endpoint::{Endpoint, Method, Service};
pub use file_content::{
    FileContent, FileContentLink, FileDownload, FileRange, MAX_RANGE, MAX_STREAM, StreamStart, is_content_link,
};
pub use frame::{
    AcceptanceAcknowledgement, Authenticate, ConnectParams, DeliveryResponse, Disconnect, DroppedIndicator, FrameError,
    FrameSink, FrameText, FrameWriter, InvitationReceipt, MessageLossAck, OwnFrame, PermittedResponse, Ping,
    RespondError, ResponseDecider, ResponseRefused, Sent,
};
pub use giphy::{GiphyPicture, InlinePicture};
pub use http::{
    AMS_HOST_SUFFIX, BuildError, CONNECT_TIMEOUT, CSA_AUDIENCE, Credentials, DEFAULT_RETRY_WAIT, EMOTICONS_HOST,
    Failure, FileBody, GIPHY_HOST, GRAPH_AUDIENCE, GRAPH_HOST, HttpError, HttpTransport, IC3_AUDIENCE, MAX_BODY,
    MAX_PICTURE_BODY, MAX_RETRY_WAIT, MIDDLE_TIER_AUDIENCE, PRESENCE_AUDIENCE, PROVISIONING_AUDIENCE, RELAY_HOST,
    REQUEST_TIMEOUT, Refusal, STREAM_TIMEOUT, TROUTER_HOST,
};
pub use meeting::{MeetingCode, MeetingDetails, MeetingThread};
#[cfg(feature = "test-support")]
pub use own::ProbeOwnState;
pub use own::{
    CALLING_APP_ID, CALLING_TEMPLATE, CLIENT_BUILD, CallingRegistration, EndpointRegistrar, EndpointRegistration,
    MESSAGING_APP_ID, MESSAGING_TEMPLATE, OwnState, REGISTRATION_TTL, RegisteredEndpoint, TROUTER_DOMAIN, register,
    register_calling,
};
#[cfg(feature = "test-support")]
pub use post_read::ProbePostRead;
pub use post_read::{
    AnswerError, AuthorizedPostRead, PRESENCE_PUSH_PATH, PostRead, PostReadMismatch, PresenceRead,
    PresenceSubscription, ShortProfile, ShortProfileLookup, SubscriptionStatus, UserPresence,
};
pub use read::{
    CSA_TEAMS_PATH, CSA_TEAMS_QUERY, Channel, ChannelMeetup, ChannelType, CsaAnswerError, CsaTeams, GRAPH_PICTURE_SIZE,
    GRAPH_USER_SELECT, GraphAnswerError, GraphPicture, GraphUser, MAX_PICTURE, Picture, PictureFormat, Read, Team,
    TeamAbout, TeamsAnswer, object_id,
};
pub use recurrence::{Pattern as RecurrencePattern, Recurrence, WeekIndex, Weekday};
pub use relay::{RelayAnswer, RelayAnswerError, RelayCredential, RelayTokens};
pub use request::Request;
pub use schedule::{
    AVAILABILITY_DAYS, BusyTime, FreeBusy, FreeBusyAsk, MeetingTimes, PersonSchedule, SUGGESTION_DAYS, Schedules,
};
pub use search::{
    ALL_PAGE, Found, FoundChannel, FoundChat, FoundFile, FoundMessage, FoundPerson, PAGE, SUBSTRATE_AUDIENCE,
    SUBSTRATE_HOST, SearchAnswerError, SearchKind, SubstrateSearch, highlighted,
};
pub use team_apps::{
    AppDefinitions, AppStore, DEFINITIONS_PAGE, DefaultTeamPicture, InstalledApp, OwnedGroups, StoreApp,
    TEMPLATE_LOCALE, TeamApps, TeamTemplateCard, TeamTemplateDetails, TeamTemplateRead, TeamTemplates, TemplateChannel,
};
pub use team_read::{
    AddablePeopleSearch, AllTeamsAnalyticsRead, AnalyticsPeriod, ChannelAnalyticsRead, ChannelOwnProperty,
    JoinableTeam, JoinableTeams, ManagedChannel, OwnTeamView, PendingInvite, PendingInvites, RosterMember, TagsRead,
    TeamAnalytics, TeamAnalyticsRead, TeamChannels, TeamCode, TeamJoinRequests, TeamPicture, TeamRoster, TeamTag,
    TeamTags, TeamThread,
};
pub use thumbnail::{FileThumbnails, THUMBNAIL_HOST_SUFFIX, ThumbnailLink, is_thumbnail_link};
pub use urlp::LinkPreviewPicture;
pub use write::{
    AcceptCall, AcceptParts, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts, AddToCall, AnswerMedia,
    AnswerMediaParts, AppDefinition, ApplyChannelParameters, AttachCall, AttachParts, Author, Authorized,
    CALL_START_URL, CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, CallWriteError,
    ChannelParameters, ClientInfo, CreateChat, DecideLobby, DeclineCall, DeleteMessage, EditMessage, ForceAvailability,
    Invitee, JoinCall, JoinMedia, JoinParts, KeepAliveCall, LeaveCall, LeaveReason, Links, ManualAvailability,
    MediaContent, MediaDescription, MediaDescriptions, MeetingInfo, MemberUpdate, Mutating, NewTeam, Original, RawBody,
    ReadReceipt, RemoveReaction, RenegotiateMedia, ReportRinging, SendMessage, SetEndpointAvailability,
    SetMeetingTitle, SetReaction, StartCall, StartParts, TEAMS_ECHO_BOT_MRI, TEST_CALL_SCENARIO, TargetMismatch,
    TeamCopy, TeamRole, TeamWrite, TemplateTeam, Typing, UpdateEndpointMetadata, UpdateEndpointState,
    UpdateMediaDescriptions, UpdateParticipantProperties, VoiceConsent, Wire, team_created, team_error_code,
};
pub use write::{
    AddChannelMembers, ChannelCreated, ChannelLayout, CreateChannel, NewChannel, NewChannelKind, ShowChannelByDefault,
    channel_error_code,
};
pub use write::{
    CancelChannelEvent, CancelEvent, CreateChannelEvent, CreateEvent, CreateOnlineMeeting, DeleteEvent, EventAttendee,
    EventDraft, EventShowAs, MeetingChannel, Organizer, UpdateChannelEvent, UpdateEvent, ZonedTime,
};

pub use workflow::{WorkflowFailure, WorkflowRequest, WorkflowResponse};

/// Deterministic work-account one-to-one conversation identity.
pub use teams_lab_targets::one_to_one_id;
