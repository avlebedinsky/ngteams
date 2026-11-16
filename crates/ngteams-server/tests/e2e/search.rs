//! The title bar's search: the client engine's service search (`teams_core::service_search`) against the server's
//! Substrate search, its suggestions and its results, over the synthetic office below.

use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_core::{SearchAnswer, SearchScope};
use teams_proto::chat::MessageText;
use teams_proto::{Endpoint, Method, Mri, Request, SUBSTRATE_AUDIENCE, SearchKind, Service};
use teams_store::OutboxState;

use crate::support::{Backend, TestServer, backends};
use crate::teams::{Seat, server};

async fn send(seat: &Seat, conversation: &str, text: &str) -> String {
    let row = seat
        .engine
        .write(Action::Send {
            conversation: conversation.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

async fn chat(seat: &Seat, others: &[&Seat], topic: Option<&str>) -> String {
    let row = seat
        .engine
        .write(Action::CreateChat {
            others: others.iter().map(|other| Mri::new(&other.mri)).collect(),
            topic: topic.map(str::to_owned),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

/// A member's removal from a group chat, as the chat service takes it.
struct RemoveMember {
    chat: String,
    member: String,
}

impl Request for RemoveMember {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Delete
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", &self.chat, "members", &self.member])
    }
    fn body(&self) -> Option<Value> {
        None
    }
    fn is_write(&self) -> bool {
        true
    }
}

async fn search(seat: &Seat, query: &str, scope: SearchScope) -> SearchAnswer {
    seat.engine.search_service(query, scope).await.unwrap()
}

/// Ada, Bert and Cora (an engineer in Research) in a group chat "Picnic crew"; Ada's team "Rocket Team" with Bert,
/// with a channel "Launch Plans"; Dan and Cora in a chat of their own. Every one of them an engine.
struct Office {
    server: TestServer,
    ada: Seat,
    bert: Seat,
    cora: Seat,
    dan: Seat,
    picnic: String,
    team: String,
    launch: String,
    secret: String,
}

impl Office {
    async fn open(backend: Backend) -> Self {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada Lovelace").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert Sample").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora Chief").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan Fourth").await;
        let profile = ngteams_server::people::Profile {
            user_id: cora.id.clone(),
            job_title: Some("Engineer".into()),
            department: Some("Research".into()),
            ..ngteams_server::people::Profile::default()
        };
        ngteams_server::people::repo::put_profile(&server.app.db, &profile)
            .await
            .unwrap();
        let picnic = chat(&ada, &[&bert, &cora], Some("Picnic crew")).await;
        let (team, group) = ada.make_team("Rocket Team", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let launch = ada.make_channel(&team, &group, "Launch Plans", true).await;
        let secret = chat(&dan, &[&cora], None).await;
        Self {
            server,
            ada,
            bert,
            cora,
            dan,
            picnic,
            team,
            launch,
            secret,
        }
    }

    async fn stop(self) {
        for seat in [self.ada, self.bert, self.cora, self.dan] {
            seat.stop();
        }
        self.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_title_bar_search_finds_messages_people_chats_and_channels() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let review = send(
            &office.bert,
            &office.picnic,
            "The quarterly <rocket> review is on Friday",
        )
        .await;
        send(&office.ada, &office.launch, "Countdown starts at noon, rockets ready").await;
        send(&office.dan, &office.secret, "the rocket secret stays here").await;
        send(&office.ada, &office.picnic, "Bring sandwiches").await;

        // All: a few of each kind, only what Ada may read
        let all = search(&office.ada, "rocket", SearchScope::All).await;
        let mut conversations: Vec<&str> = all.messages.iter().map(|found| found.conversation.as_str()).collect();
        conversations.sort_unstable();
        let mut expected = vec![office.picnic.as_str(), office.launch.as_str()];
        expected.sort_unstable();
        assert_eq!(conversations, expected, "{:?}", all.messages);
        let found = all.messages.iter().find(|found| found.id == review).unwrap();
        assert_eq!(found.sender_mri.as_deref(), Some(office.bert.mri.as_str()));
        assert_eq!(found.sender_name.as_deref(), Some("Bert Sample"));
        assert_eq!(found.snippet, "The quarterly <rocket> review is on Friday");
        assert_eq!(&found.snippet[found.matched[0].clone()], "rocket");
        assert_eq!(found.sent_ms, review.parse::<i64>().unwrap());
        assert_eq!(found.parent, None);
        let channel_post = all
            .messages
            .iter()
            .find(|found| found.conversation == office.launch)
            .unwrap();
        assert_eq!(channel_post.parent.as_deref(), Some(channel_post.id.as_str()));
        assert_eq!(all.channels.len(), 1, "{:?}", all.channels);
        assert_eq!(all.channels[0].name, "Rocket Team");
        assert_eq!(all.channels[0].id, office.team);
        assert_eq!(all.channels[0].team, None);

        // People: by a name's start and by address, with what the card opens by
        let people = search(&office.ada, "cor", SearchScope::Only(SearchKind::People)).await;
        assert_eq!(people.people.len(), 1, "{:?}", people.people);
        let cora = &people.people[0];
        assert_eq!(cora.mri.as_deref(), Some(office.cora.mri.as_str()));
        assert_eq!(cora.name, "Cora Chief");
        assert_eq!(cora.email.as_deref(), Some("cora@own.test"));
        assert_eq!(cora.job_title.as_deref(), Some("Engineer"));
        assert_eq!(cora.department.as_deref(), Some("Research"));
        let by_address = search(&office.ada, "bert@own", SearchScope::Only(SearchKind::People)).await;
        assert_eq!(by_address.people.len(), 1);
        assert_eq!(by_address.people[0].name, "Bert Sample");

        // Group chats: by topic and by a member's name; a chat of others is no find
        let chats = search(&office.ada, "picnic", SearchScope::Only(SearchKind::Chats)).await;
        assert_eq!(chats.chats.len(), 1, "{:?}", chats.chats);
        assert_eq!(chats.chats[0].id, office.picnic);
        assert_eq!(chats.chats[0].name, "Picnic crew");
        let by_member = search(&office.bert, "cora", SearchScope::Only(SearchKind::Chats)).await;
        assert_eq!(by_member.chats.len(), 1, "{:?}", by_member.chats);
        assert_eq!(by_member.chats[0].id, office.picnic);
        let outsider = search(&office.ada, "dan", SearchScope::Only(SearchKind::Chats)).await;
        assert!(outsider.chats.is_empty(), "{:?}", outsider.chats);

        // Channels: a channel with its team's name; a team the reader isn't on is no find
        let channels = search(&office.bert, "launch", SearchScope::Only(SearchKind::Channels)).await;
        assert_eq!(channels.channels.len(), 1, "{:?}", channels.channels);
        assert_eq!(channels.channels[0].id, office.launch);
        assert_eq!(channels.channels[0].name, "Launch Plans");
        assert_eq!(channels.channels[0].team.as_deref(), Some("Rocket Team"));
        let not_on = search(&office.cora, "launch", SearchScope::Only(SearchKind::Channels)).await;
        assert!(not_on.channels.is_empty(), "{:?}", not_on.channels);

        // the suggestions as the query is typed
        let typed = search(&office.ada, "ro", SearchScope::Suggestions).await;
        assert!(typed.channels.iter().any(|found| found.id == office.team), "{typed:?}");
        assert!(typed.messages.iter().any(|found| found.id == review), "{typed:?}");
        let typed = search(&office.ada, "bert s", SearchScope::Suggestions).await;
        assert_eq!(typed.people.len(), 1, "{typed:?}");
        assert_eq!(typed.people[0].mri.as_deref(), Some(office.bert.mri.as_str()));
        assert!(typed.chats.iter().any(|found| found.id == office.picnic), "{typed:?}");

        // the user's text is never the service's syntax
        let odd = search(&office.ada, "\"rocket*) (:", SearchScope::All).await;
        assert_eq!(odd.messages.len(), 2, "{:?}", odd.messages);
        assert_eq!(
            search(&office.ada, "zzqx", SearchScope::All).await,
            SearchAnswer::default()
        );
        office.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_search_follows_edits_deletes_and_who_may_read_the_history() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let before = send(&office.ada, &office.picnic, "Meet at the lighthouse").await;
        let gone = send(&office.ada, &office.picnic, "the password is swordfish").await;
        assert_eq!(
            search(&office.cora, "lighthouse", SearchScope::All)
                .await
                .messages
                .len(),
            1
        );

        let edited = office
            .ada
            .engine
            .write(Action::Edit {
                conversation: office.picnic.clone(),
                message_id: before.clone(),
                text: MessageText::plain("Meet at the harbour").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(edited.state, OutboxState::Sent, "{edited:?}");
        assert!(
            search(&office.cora, "lighthouse", SearchScope::All)
                .await
                .messages
                .is_empty()
        );
        let harbour = search(&office.cora, "harbour", SearchScope::All).await;
        assert_eq!(harbour.messages.len(), 1);
        assert_eq!(harbour.messages[0].id, before);

        let deleted = office
            .ada
            .engine
            .write(Action::Delete {
                conversation: office.picnic.clone(),
                message_id: gone,
            })
            .await
            .unwrap();
        assert_eq!(deleted.state, OutboxState::Sent, "{deleted:?}");
        assert!(
            search(&office.bert, "swordfish", SearchScope::All)
                .await
                .messages
                .is_empty()
        );

        // Dan, added later without the history, finds the chat but none of what was said before him
        let added = office
            .ada
            .engine
            .write(Action::AddMembers {
                conversation: office.picnic.clone(),
                members: vec![Mri::new(&office.dan.mri)],
                history: teams_core::engine::ChatHistory::None,
            })
            .await
            .unwrap();
        assert_eq!(added.state, OutboxState::Sent, "{added:?}");
        assert!(
            search(&office.dan, "harbour", SearchScope::All)
                .await
                .messages
                .is_empty()
        );
        let later = send(&office.dan, &office.picnic, "the harbour has fog").await;
        let found = search(&office.dan, "harbour", SearchScope::All).await;
        assert_eq!(found.messages.len(), 1, "{:?}", found.messages);
        assert_eq!(found.messages[0].id, later);

        office.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_who_left_keeps_what_they_could_read_but_not_the_chat_or_who_is_in_it() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let before = send(&office.ada, &office.picnic, "Meet at the harbour").await;
        let removed = office
            .ada
            .client
            .write(RemoveMember {
                chat: office.picnic.clone(),
                member: office.cora.mri.clone(),
            })
            .await
            .unwrap();
        assert!(removed.is_success(), "{}", removed.status());
        let after = send(&office.ada, &office.picnic, "the kite is in the harbour").await;

        // Cora finds what she could read, without the chat's people of now; nothing said after; not the chat
        let kept = search(&office.cora, "harbour", SearchScope::All).await;
        assert_eq!(kept.messages.len(), 1, "{:?}", kept.messages);
        assert_eq!(kept.messages[0].id, before);
        assert_eq!(kept.messages[0].to, None, "{:?}", kept.messages[0]);
        assert!(search(&office.cora, "kite", SearchScope::All).await.messages.is_empty());
        let chats = search(&office.cora, "picnic", SearchScope::Only(SearchKind::Chats)).await;
        assert!(chats.chats.is_empty(), "{:?}", chats.chats);

        // Bert, still in it, finds both, with its people of now
        let seated = search(&office.bert, "harbour", SearchScope::All).await;
        let ids: Vec<&str> = seated.messages.iter().map(|found| found.id.as_str()).collect();
        assert_eq!(ids, [after.as_str(), before.as_str()], "{:?}", seated.messages);
        let to = seated.messages[0].to.as_deref().unwrap_or_default();
        assert!(to.contains("Ada Lovelace") && !to.contains("Cora"), "{to}");
        office.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tab_pages_many_finds_newest_first_and_says_more_are_left() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let mut sent = Vec::new();
        for n in 0..(teams_proto::PAGE + 3) {
            sent.push(send(&office.bert, &office.picnic, &format!("status report number {n}")).await);
        }
        let tab = search(&office.ada, "status report", SearchScope::Only(SearchKind::Messages)).await;
        assert_eq!(tab.messages.len(), teams_proto::PAGE as usize);
        assert!(tab.more);
        assert_eq!(tab.messages[0].id, *sent.last().unwrap());
        let all = search(&office.ada, "report", SearchScope::All).await;
        assert_eq!(all.messages.len(), teams_proto::ALL_PAGE as usize);
        assert!(all.more);
        let exact = search(&office.ada, "number 7", SearchScope::Only(SearchKind::Messages)).await;
        assert_eq!(exact.messages.len(), 1, "{:?}", exact.messages);
        assert!(!exact.more);
        office.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_the_reader_owns_or_was_sent_are_found_by_name() {
    for backend in backends() {
        let trio = crate::media::Trio::start(backend).await;
        crate::media::files::send_file(&trio, "Budget forecast.xlsx", b"synthetic sheet").await;
        let mine = trio
            .a
            .search_service("budget", SearchScope::Only(SearchKind::Files))
            .await
            .unwrap();
        assert_eq!(mine.files.len(), 1, "{:?}", mine.files);
        let file = &mine.files[0];
        assert_eq!(file.name, "Budget forecast.xlsx");
        assert_eq!(file.kind.as_deref(), Some("xlsx"));
        assert!(
            file.url.starts_with("https://own.example.test/sp/personal/"),
            "{}",
            file.url
        );
        assert!(file.modified_ms.is_some());
        let shared = trio.b.search_service("forecast", SearchScope::All).await.unwrap();
        assert_eq!(shared.files.len(), 1, "{:?}", shared.files);
        let dir = tempfile::tempdir().unwrap();
        let cora = crate::media::engine(&trio.server, "c@own.test", &trio.ids[2], dir.path()).await;
        let outsider = cora
            .search_service("budget", SearchScope::Only(SearchKind::Files))
            .await
            .unwrap();
        assert!(outsider.files.is_empty(), "{:?}", outsider.files);
        cora.shutdown();
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_of_teams_and_channels_runs_on_from_the_teams_into_the_channels() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let (team, group) = office.ada.make_team("Rocket Science", false).await;
        office.ada.make_channel(&team, &group, "Rocket Fuel", true).await;
        let session = office.server.session("ada@own.test").await;
        let token = session
            .access_token(SUBSTRATE_AUDIENCE)
            .await
            .unwrap()
            .token
            .expose()
            .to_owned();
        let page = |from: u32| {
            let request = reqwest::Client::new()
                .post(office.server.url("/search/searchservice/api/v2/query"))
                .bearer_auth(&token)
                .json(&json!({"EntityRequests": [{"entityType": "TeamsChannel",
                    "query": {"queryString": "rocket"}, "from": from, "size": 1}]}));
            async move {
                let answer: Value = request.send().await.unwrap().json().await.unwrap();
                let set = answer["EntitySets"][0]["ResultSets"][0].clone();
                let names: Vec<String> = set["Results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|result| result["Source"]["Name"].as_str().unwrap().to_owned())
                    .collect();
                (names, set["MoreResultsAvailable"].as_bool().unwrap())
            }
        };
        // the two teams by name, then the channel
        assert_eq!(page(0).await, (vec!["Rocket Science".to_owned()], true));
        assert_eq!(page(1).await, (vec!["Rocket Team".to_owned()], true));
        assert_eq!(page(2).await, (vec!["Rocket Fuel".to_owned()], false));
        office.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_search_takes_only_its_own_token_and_refuses_an_empty_ask() {
    for backend in backends() {
        let server = server(backend).await;
        server.add_user("ada@own.test", "Ada").await;
        let session = server.session("ada@own.test").await;
        let token = |audience: &'static str| {
            let session = &session;
            async move { session.access_token(audience).await.unwrap().token.expose().to_owned() }
        };
        let post = |token: Option<String>, body: Value| {
            let mut request = reqwest::Client::new()
                .post(server.url("/search/searchservice/api/v2/query"))
                .json(&body);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            async move { request.send().await.unwrap().status().as_u16() }
        };
        let ask = json!({"EntityRequests": [{"entityType": "People", "query": {"queryString": "ada"}, "size": 5}]});
        assert_eq!(post(None, ask.clone()).await, 401);
        assert_eq!(
            post(Some(token(teams_proto::GRAPH_AUDIENCE).await), ask.clone()).await,
            401
        );
        assert_eq!(post(Some(token(SUBSTRATE_AUDIENCE).await), ask).await, 200);
        // the web client's empty `PowerbarContextual` ask is answered 400, as Substrate answers it
        assert_eq!(
            post(Some(token(SUBSTRATE_AUDIENCE).await), json!({"EntityRequests": []})).await,
            400
        );
        server.stop().await;
    }
}
