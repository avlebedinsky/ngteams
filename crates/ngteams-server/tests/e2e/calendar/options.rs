//! A meeting's options and the room finder's reads: the client's typed requests against the middle tier.

use teams_core::EventWrite;
use teams_core::engine::Action;
use teams_proto::{
    MeetingOption, MeetingOptions, MeetingOptionsRead, Mri, OptionsOf, Place, RoomLists, Rooms, SetMeetingOption,
};

use super::meetings::invited;
use super::{HOUR, MONDAY, Seat, draft};
use crate::support::{TestServer, backends};
use crate::teams::ORIGIN;

async fn options(seat: &Seat, of: &OptionsOf) -> Result<MeetingOptions, u16> {
    match seat.client.read(&MeetingOptionsRead(of.clone())).await {
        Ok(answer) => Ok(MeetingOptions::read_answer(answer.body()).unwrap()),
        Err(teams_proto::HttpError::Status(answer)) => Err(answer.status()),
        Err(error) => panic!("{error}"),
    }
}

async fn set(seat: &Seat, of: &OptionsOf, option: MeetingOption) -> Result<MeetingOptions, u16> {
    let write = SetMeetingOption::new(of.clone(), option);
    let answer = seat.client.write(write).await.unwrap();
    match answer.status() {
        200 => Ok(MeetingOptions::read_answer(answer.body()).unwrap()),
        status => Err(status),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_meetings_options_are_read_by_the_people_in_it_and_saved_by_its_organizer() {
    for backend in backends() {
        let server = super::server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan").await;
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();
        let mut planning = draft("Planning", MONDAY + 9 * HOUR, 30);
        planning.online = true;
        planning.attendees = vec![invited("bert@own.test", "Bert", None, true)];
        let written = ada.engine.write_event(EventWrite::Create(planning)).await.unwrap();
        let chat = written.created.unwrap().meeting_chat.unwrap();
        let of = OptionsOf::new(&tenant, &ada.id, &chat).unwrap();

        // as Teams sets a new meeting: a lobby for outsiders, everyone presents, chats and uses their devices
        let all = MeetingOptions {
            lobby: Some(true),
            present: Some(true),
            chat: Some(true),
            unmute: Some(true),
            camera: Some(true),
        };
        assert_eq!(options(&ada, &of).await, Ok(all));
        assert_eq!(options(&bert, &of).await, Ok(all));

        // each change answers the option as it now is, and is kept
        let changed = set(&ada, &of, MeetingOption::Lobby(false)).await.unwrap();
        assert_eq!(changed.lobby, Some(false));
        let changed = set(&ada, &of, MeetingOption::Present(false)).await.unwrap();
        assert_eq!(changed.present, Some(false));
        let changed = set(&ada, &of, MeetingOption::Chat(false)).await.unwrap();
        assert_eq!(changed.chat, Some(false));
        let restricted = MeetingOption::Attendees {
            unmute: false,
            camera: true,
        };
        let changed = set(&ada, &of, restricted).await.unwrap();
        assert_eq!((changed.unmute, changed.camera), (Some(false), Some(true)));
        let kept = MeetingOptions {
            lobby: Some(false),
            present: Some(false),
            chat: Some(false),
            unmute: Some(false),
            camera: Some(true),
        };
        assert_eq!(options(&bert, &of).await, Ok(kept));
        let both = MeetingOption::Attendees {
            unmute: false,
            camera: false,
        };
        let changed = set(&ada, &of, both).await.unwrap();
        assert_eq!((changed.unmute, changed.camera), (Some(false), Some(false)));

        // someone invited reads them and changes none; someone outside the meeting learns nothing of it
        assert_eq!(set(&bert, &of, MeetingOption::Lobby(true)).await, Err(403));
        assert_eq!(options(&dan, &of).await, Err(404));
        assert_eq!(set(&dan, &of, MeetingOption::Lobby(true)).await, Err(404));
        assert_eq!(options(&bert, &of).await.unwrap().lobby, Some(false));
        // the meeting is named by its organizer and tenant: another's names nothing
        let not_organizer = OptionsOf::new(&tenant, &bert.id, &chat).unwrap();
        assert_eq!(options(&ada, &not_organizer).await, Err(404));
        let elsewhere = OptionsOf::new("00000000-0000-4000-8000-000000000000", &ada.id, &chat).unwrap();
        assert_eq!(options(&ada, &elsewhere).await, Err(404));

        // a group chat's call has options too: whoever in the chat is named as its organizer saves them
        let group = ada
            .engine
            .write(Action::CreateChat {
                others: vec![Mri::new(&bert.mri), Mri::new(&dan.mri)],
                topic: Some("Crew".into()),
            })
            .await
            .unwrap()
            .message_id
            .unwrap();
        let of_bert = OptionsOf::new(&tenant, &bert.id, &group).unwrap();
        assert_eq!(options(&dan, &of_bert).await, Ok(all));
        assert_eq!(set(&dan, &of_bert, MeetingOption::Chat(false)).await, Err(403));
        assert_eq!(
            set(&bert, &of_bert, MeetingOption::Chat(false)).await.unwrap().chat,
            Some(false)
        );
        assert_eq!(options(&ada, &of_bert).await.unwrap().chat, Some(false));
        for seat in [ada, bert, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_room_finder_reads_the_rooms_the_server_is_configured_with() {
    use ngteams_server::config::RoomConfig;
    for backend in backends() {
        let room = |name: &str, list: Option<&str>, capacity: u32| RoomConfig {
            name: name.into(),
            address: format!("{}@own.example.test", name.to_lowercase()),
            list: list.map(str::to_owned),
            capacity: Some(capacity),
            floor: Some("2".into()),
            street: Some("1 Example Way".into()),
            display_device: Some("Display".into()),
            ..RoomConfig::default()
        };
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse(ORIGIN).unwrap();
            config.rooms = vec![
                room("Apollo", Some("Main building"), 8),
                room("Gemini", Some("Main building"), 4),
                room("Shed", None, 2),
            ];
        })
        .await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let read_lists = ada.client.read(&RoomLists).await.unwrap();
        let lists = RoomLists::read_answer(read_lists.body()).unwrap();
        assert_eq!(lists.len(), 1, "{lists:?}");
        assert_eq!(lists[0].name, "Main building");
        assert_eq!(lists[0].address, "main-building@own.example.test");

        let rooms = |list: Option<String>| {
            let client = &ada.client;
            async move {
                let answer = client.read(&Rooms { list }).await.unwrap();
                Rooms::read_answer(answer.body()).unwrap()
            }
        };
        let names = |places: &[Place]| places.iter().map(|place| place.name.clone()).collect::<Vec<_>>();
        let all = rooms(None).await;
        assert_eq!(names(&all), ["Apollo", "Gemini", "Shed"]);
        assert_eq!(all[0].address, "apollo@own.example.test");
        assert_eq!(all[0].capacity, Some(8));
        assert_eq!(all[0].floor.as_deref(), Some("2"));
        assert_eq!(all[0].building.as_deref(), Some("Main building"));
        assert_eq!(all[0].street.as_deref(), Some("1 Example Way"));
        assert_eq!(all[0].features, ["Display"]);
        assert_eq!(
            names(&rooms(Some(lists[0].address.clone())).await),
            ["Apollo", "Gemini"]
        );
        assert!(rooms(Some("nowhere@own.example.test".into())).await.is_empty());

        // a room is a place, not an account: an event that names one as an attendee invites nobody by it
        let mut review = draft("Review", MONDAY + 9 * HOUR, 30);
        review.location = "Apollo".into();
        review.attendees = vec![invited("apollo@own.example.test", "Apollo", None, true)];
        let created = ada.engine.write_event(EventWrite::Create(review)).await.unwrap();
        let whole = ada.engine.calendar_event(created.created.unwrap().id).await.unwrap();
        assert!(whole.event.attendees.is_empty());
        ada.stop();
        server.stop().await;

        // a server with no rooms answers none
        let server = super::server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let read_lists = ada.client.read(&RoomLists).await.unwrap();
        assert!(RoomLists::read_answer(read_lists.body()).unwrap().is_empty());
        let read_rooms = ada.client.read(&Rooms::default()).await.unwrap();
        assert!(Rooms::read_answer(read_rooms.body()).unwrap().is_empty());
        ada.stop();
        server.stop().await;
    }
}
