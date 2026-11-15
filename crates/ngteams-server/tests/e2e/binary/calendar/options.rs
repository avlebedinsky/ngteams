//! A meeting's options (its Controls): read by the people in it, saved only by its organizer, and nothing of them for
//! someone outside it.

use teams_proto::{MeetingOption, MeetingOptions, MeetingOptionsRead, OptionsOf, SetMeetingOption};

use super::{Trio, User, create, invite};
use crate::calendar::{HOUR, MONDAY, draft};
use crate::support::backends;

async fn options(user: &User, of: &OptionsOf) -> Result<MeetingOptions, u16> {
    match user.client().read(&MeetingOptionsRead(of.clone())).await {
        Ok(answer) => Ok(MeetingOptions::read_answer(answer.body()).unwrap()),
        Err(teams_proto::HttpError::Status(answer)) => Err(answer.status()),
        Err(error) => panic!("{error}"),
    }
}

async fn set(user: &User, of: &OptionsOf, option: MeetingOption) -> Result<MeetingOptions, u16> {
    let answer = user
        .client()
        .write(SetMeetingOption::new(of.clone(), option))
        .await
        .unwrap();
    match answer.status() {
        200 => Ok(MeetingOptions::read_answer(answer.body()).unwrap()),
        status => Err(status),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meetings_options_are_read_by_its_people_and_saved_by_its_organizer_only() {
    for backend in backends() {
        let mut trio = Trio::start(backend).await;
        let d = trio.client("d@own.test", false).await;
        let mut planning = draft("Planning", MONDAY + 9 * HOUR, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b), invite(&trio.c)];
        let created = create(&trio.a, planning).await;
        let chat = created.meeting_chat.clone().unwrap();
        let info = created.meeting.clone().unwrap();
        let of = OptionsOf::new(&info.tenant_id, &info.organizer_id, &chat).unwrap();

        let defaults = MeetingOptions {
            lobby: Some(true),
            present: Some(true),
            chat: Some(true),
            unmute: Some(true),
            camera: Some(true),
        };
        for user in [&trio.a, &trio.b, &trio.c] {
            assert_eq!(options(user, &of).await, Ok(defaults), "{}", user.upn);
        }
        // the organizer saves; b and c, at the other node, read what was saved and save nothing
        assert_eq!(
            set(&trio.a, &of, MeetingOption::Lobby(false)).await.unwrap().lobby,
            Some(false)
        );
        let quiet = MeetingOption::Attendees {
            unmute: false,
            camera: true,
        };
        let saved = set(&trio.a, &of, quiet).await.unwrap();
        assert_eq!((saved.unmute, saved.camera), (Some(false), Some(true)));
        for user in [&trio.b, &trio.c] {
            assert_eq!(
                set(user, &of, MeetingOption::Lobby(true)).await,
                Err(403),
                "{}",
                user.upn
            );
            assert_eq!(
                set(user, &of, MeetingOption::Chat(false)).await,
                Err(403),
                "{}",
                user.upn
            );
        }
        let kept = MeetingOptions {
            lobby: Some(false),
            present: Some(true),
            chat: Some(true),
            unmute: Some(false),
            camera: Some(true),
        };
        assert_eq!(options(&trio.b, &of).await, Ok(kept));
        // d, outside the meeting, learns nothing and changes nothing
        assert_eq!(options(&d, &of).await, Err(404));
        assert_eq!(set(&d, &of, MeetingOption::Lobby(true)).await, Err(404));
        // b's own name as organizer names no meeting
        let as_b = OptionsOf::new(&info.tenant_id, trio.b.oid(), &chat).unwrap();
        assert_eq!(set(&trio.b, &as_b, MeetingOption::Lobby(true)).await, Err(404));
        // kept across a restart of the server
        trio.rig.restart().await;
        trio.b.restart().await;
        assert_eq!(options(&trio.c, &of).await, Ok(kept));
        drop(d);
        trio.stop().await;
    }
}
