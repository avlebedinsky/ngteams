//! The credential broker on a socket pair: the client store gets, puts and deletes the scope's own items through the
//! broker's store, anything outside the scope is refused and reaches no store, a malformed or oversized frame on either
//! side closes the socket, the peer closing while the store waits ends the broker at once, and no value reaches a
//! `Debug`, an error or a note. The broker's store is a `MemorySecretStore` (or a failing or waiting one): nothing here
//! reaches a keyring or a bus.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};

use teams_auth::broker::{BrokerError, BrokerScope, BrokerSecretStore, MAX_FRAME, Note, ServeError, serve};
use teams_auth::secrets::{MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};
use tokio::task::JoinHandle;

const APPLICATION: &str = "teams-lab";
const ACCOUNT: &str = "a@lab.example.test";
const OTHER: &str = "b@lab.example.test";
const SPACES: &str = "https://api.spaces.skype.com";
const PLANTED: &str = "planted-secret-value-0123456789";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn scope() -> BrokerScope {
    BrokerScope::new(APPLICATION, ACCOUNT)
}

fn key(account: &str, audience: &str) -> SecretKey {
    SecretKey::new(account, audience)
}

/// The broker's end of a socket pair, for tokio, and the client's, blocking.
fn socket_pair() -> TestResult<(tokio::net::UnixStream, UnixStream)> {
    let (server, client) = UnixStream::pair()?;
    server.set_nonblocking(true)?;
    Ok((tokio::net::UnixStream::from_std(server)?, client))
}

/// A broker serving `store` on one end, its notes (as shown) kept.
struct Broker {
    served: JoinHandle<Result<(), ServeError>>,
    notes: Arc<Mutex<Vec<String>>>,
}

impl Broker {
    fn start<S: SecretStore + 'static>(store: Arc<S>, server: tokio::net::UnixStream) -> Self {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&notes);
        let serving = tokio::spawn(async move {
            serve(server, &*store, &scope(), |note: &Note<'_>| {
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend([note.to_string(), format!("{note:?}")]);
            })
            .await
        });
        Self { served: serving, notes }
    }

    fn notes(&self) -> Vec<String> {
        self.notes.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    async fn end(self) -> TestResult<Result<(), ServeError>> {
        Ok(self.served.await?)
    }
}

/// A client in `application` over a broker for [`scope`] serving `store`.
fn connect(store: &Arc<MemorySecretStore>, application: &str) -> TestResult<(BrokerSecretStore, Broker)> {
    let (server, client) = socket_pair()?;
    Ok((
        BrokerSecretStore::new(client, application)?,
        Broker::start(Arc::clone(store), server),
    ))
}

async fn value(store: &impl SecretStore, account: &str, audience: &str) -> Result<Option<String>, SecretError> {
    Ok(store
        .get(&key(account, audience))
        .await?
        .map(|secret| secret.expose().to_owned()))
}

fn refused<T: std::fmt::Debug>(result: &Result<T, SecretError>) -> bool {
    matches!(result, Err(SecretError::Broker(BrokerError::Refused)))
}

#[tokio::test]
async fn the_scopes_own_items_are_got_put_and_deleted_in_the_brokers_store() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    let (client, broker) = connect(&backing, APPLICATION)?;

    assert_eq!(value(&client, ACCOUNT, SPACES).await?, None);
    client.put(&key(ACCOUNT, SPACES), &Secret::new("spaces-1")).await?;
    client.put(&key(ACCOUNT, "skypetoken"), &Secret::new("skype-1")).await?;
    assert_eq!(value(&client, ACCOUNT, SPACES).await?.as_deref(), Some("spaces-1"));
    assert_eq!(value(&*backing, ACCOUNT, SPACES).await?.as_deref(), Some("spaces-1"));
    client.put(&key(ACCOUNT, SPACES), &Secret::new("spaces-2")).await?;
    assert_eq!(value(&*backing, ACCOUNT, SPACES).await?.as_deref(), Some("spaces-2"));
    client.delete(&key(ACCOUNT, SPACES)).await?;
    client.delete(&key(ACCOUNT, SPACES)).await?;
    assert_eq!(value(&client, ACCOUNT, SPACES).await?, None);
    assert_eq!(
        value(&*backing, ACCOUNT, "skypetoken").await?.as_deref(),
        Some("skype-1")
    );

    // an empty secret and one with any text go through as they are
    client.put(&key(ACCOUNT, "empty"), &Secret::new("")).await?;
    assert_eq!(value(&client, ACCOUNT, "empty").await?.as_deref(), Some(""));
    client.put(&key(ACCOUNT, "text"), &Secret::new("ü\n\"x\"\0")).await?;
    assert_eq!(value(&client, ACCOUNT, "text").await?.as_deref(), Some("ü\n\"x\"\0"));

    drop(client);
    broker.end().await??;
    Ok(())
}

#[tokio::test]
async fn concurrent_calls_each_get_their_own_answer() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    for audience in ["one", "two", "three", "four"] {
        backing.put(&key(ACCOUNT, audience), &Secret::new(audience)).await?;
    }
    let (client, _broker) = connect(&backing, APPLICATION)?;

    let (one, two, three, four) = tokio::join!(
        value(&client, ACCOUNT, "one"),
        value(&client, ACCOUNT, "two"),
        value(&client, ACCOUNT, "three"),
        value(&client, ACCOUNT, "four"),
    );

    assert_eq!(
        [one?, two?, three?, four?],
        [Some("one"), Some("two"), Some("three"), Some("four")].map(|value| value.map(str::to_owned))
    );
    Ok(())
}

#[tokio::test]
async fn another_account_application_or_no_audience_is_refused_and_reaches_no_store() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    backing.put(&key(OTHER, SPACES), &Secret::new("other-1")).await?;
    backing.put(&key(ACCOUNT, SPACES), &Secret::new("own-1")).await?;
    let (client, broker) = connect(&backing, APPLICATION)?;

    assert!(refused(&client.get(&key(OTHER, SPACES)).await));
    assert!(refused(&client.put(&key(OTHER, SPACES), &Secret::new(PLANTED)).await));
    assert!(refused(&client.delete(&key(OTHER, SPACES)).await));
    // the account must match exactly
    assert!(refused(&client.get(&key("A@lab.example.test", SPACES)).await));
    assert!(refused(&client.get(&key(&format!("{ACCOUNT} "), SPACES)).await));
    assert!(refused(&client.get(&key(ACCOUNT, "")).await));
    assert_eq!(value(&*backing, OTHER, SPACES).await?.as_deref(), Some("other-1"));
    // the connection stays after a refusal
    assert_eq!(value(&client, ACCOUNT, SPACES).await?.as_deref(), Some("own-1"));

    // another application, the account's own key: refused too
    let (personal, _other_broker) = connect(&backing, "ngteams")?;
    assert!(refused(&personal.get(&key(ACCOUNT, SPACES)).await));
    assert!(refused(
        &personal.put(&key(ACCOUNT, SPACES), &Secret::new(PLANTED)).await
    ));
    assert!(refused(&personal.delete(&key(ACCOUNT, SPACES)).await));
    assert_eq!(value(&*backing, ACCOUNT, SPACES).await?.as_deref(), Some("own-1"));

    let notes = broker.notes();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("refused") && note.contains(OTHER)),
        "{notes:?}"
    );
    Ok(())
}

/// A store whose every call fails.
struct Failing;

impl SecretStore for Failing {
    fn put(&self, _: &SecretKey, _: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::ready(Err(SecretError::NotUtf8))
    }

    fn get(&self, _: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send {
        std::future::ready(Err(SecretError::NotUtf8))
    }

    fn delete(&self, _: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::ready(Err(SecretError::NotUtf8))
    }
}

#[tokio::test]
async fn a_store_failure_is_answered_and_the_connection_stays() -> TestResult {
    let (server, client) = socket_pair()?;
    let client = BrokerSecretStore::new(client, APPLICATION)?;
    let broker = Broker::start(Arc::new(Failing), server);

    for _ in 0..2 {
        let error = client.get(&key(ACCOUNT, SPACES)).await.expect_err("fails");
        assert!(
            matches!(&error, SecretError::Broker(BrokerError::Store(text)) if text == "the stored secret is not UTF-8"),
            "{error:?}"
        );
    }
    let error = client
        .put(&key(ACCOUNT, SPACES), &Secret::new(PLANTED))
        .await
        .expect_err("fails");
    assert!(matches!(error, SecretError::Broker(BrokerError::Store(_))), "{error:?}");
    drop(client);
    broker.end().await??;
    Ok(())
}

#[tokio::test]
async fn a_stored_secret_too_large_for_a_frame_is_answered_as_a_failure() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    backing
        .put(&key(ACCOUNT, SPACES), &Secret::new("x".repeat(MAX_FRAME)))
        .await?;
    let (client, broker) = connect(&backing, APPLICATION)?;

    let error = client.get(&key(ACCOUNT, SPACES)).await.expect_err("too large");
    assert!(matches!(error, SecretError::Broker(BrokerError::Store(_))), "{error:?}");
    // the note tells what the client got: a failure, not a find
    let notes = broker.notes();
    assert!(
        notes.first().is_some_and(|note| note.ends_with(": the store failed")),
        "{notes:?}"
    );
    assert!(notes.iter().all(|note| !note.contains("found")), "{notes:?}");
    // the connection stays
    assert_eq!(value(&client, ACCOUNT, "missing").await?, None);
    Ok(())
}

/// A store whose every call waits forever, as one waiting on a keyring unlock prompt nobody answers.
struct Waiting;

impl SecretStore for Waiting {
    fn put(&self, _: &SecretKey, _: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::pending()
    }

    fn get(&self, _: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send {
        std::future::pending()
    }

    fn delete(&self, _: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        std::future::pending()
    }
}

/// How long a broker gets to notice its peer, far above the moment it takes.
const NOTICE: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn the_peer_closing_while_the_store_waits_ends_the_broker_at_once() -> TestResult {
    let (server, mut client) = socket_pair()?;
    let broker = Broker::start(Arc::new(Waiting), server);
    let get = request(1, &[APPLICATION.as_bytes(), ACCOUNT.as_bytes(), SPACES.as_bytes()]);
    client.write_all(&frame(&get))?;
    drop(client);

    let notes = Arc::clone(&broker.notes);
    let end = tokio::time::timeout(NOTICE, broker.end()).await??;
    assert!(end.is_ok(), "{end:?}");
    // the request went unanswered
    assert!(notes.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    Ok(())
}

#[tokio::test]
async fn a_byte_from_the_peer_while_the_store_waits_is_malformed() -> TestResult {
    let (server, mut client) = socket_pair()?;
    let broker = Broker::start(Arc::new(Waiting), server);
    let get = request(1, &[APPLICATION.as_bytes(), ACCOUNT.as_bytes(), SPACES.as_bytes()]);
    client.write_all(&frame(&get))?;
    client.write_all(&[0])?;

    let end = tokio::time::timeout(NOTICE, broker.end()).await??;
    assert!(matches!(end, Err(ServeError::Malformed)), "{end:?}");
    // the broker closed its end, unanswered
    let mut back = Vec::new();
    client.read_to_end(&mut back)?;
    assert!(back.is_empty(), "{back:?}");
    Ok(())
}

#[tokio::test]
async fn a_non_blocking_end_is_put_in_blocking_mode() -> TestResult {
    let (mut server, client) = UnixStream::pair()?;
    client.set_nonblocking(true)?;
    let client = BrokerSecretStore::new(client, APPLICATION)?;
    // a broker slow to answer "missing": a non-blocking read would find nothing yet and fail the call
    let answering = std::thread::spawn(move || {
        let mut buffer = [0; 1024];
        if server.read(&mut buffer).is_ok_and(|read| read > 0) {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = server.write_all(&frame(&[2]));
        }
        while server.read(&mut buffer).is_ok_and(|read| read > 0) {}
    });

    assert_eq!(value(&client, ACCOUNT, SPACES).await?, None);
    drop(client);
    answering.join().map_err(|_| "the answering thread panicked")?;
    Ok(())
}

#[tokio::test]
async fn a_request_too_large_for_a_frame_is_never_sent() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    let (client, broker) = connect(&backing, APPLICATION)?;

    let error = client
        .put(&key(ACCOUNT, SPACES), &Secret::new("x".repeat(MAX_FRAME)))
        .await
        .expect_err("too large");

    assert!(matches!(error, SecretError::Broker(BrokerError::TooLarge)), "{error:?}");
    assert!(broker.notes().is_empty());
    assert_eq!(value(&client, ACCOUNT, SPACES).await?, None);
    Ok(())
}

/// A frame: its length, then `body`.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

/// A field: its length, then `text`.
fn field(text: &[u8]) -> Vec<u8> {
    let mut field = u32::try_from(text.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
    field.extend_from_slice(text);
    field
}

fn request(op: u8, fields: &[&[u8]]) -> Vec<u8> {
    let mut body = vec![op];
    for text in fields {
        body.extend(field(text));
    }
    body
}

/// Sends `bytes` raw to a fresh broker and reads until it closes; the broker's end and what came back.
async fn send_raw(bytes: Vec<u8>, close_after: bool) -> TestResult<(Result<(), ServeError>, Vec<u8>)> {
    let (server, mut client) = socket_pair()?;
    let broker = Broker::start(Arc::new(MemorySecretStore::new()), server);
    let back = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        client.write_all(&bytes)?;
        if close_after {
            client.shutdown(std::net::Shutdown::Write)?;
        }
        // a close with this end's bytes still unread shows here as a reset rather than an end
        let mut back = Vec::new();
        match client.read_to_end(&mut back) {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => Ok(back),
            read => read.map(|_| back),
        }
    })
    .await??;
    let notes = broker.notes();
    let end = broker.end().await?;
    assert!(notes.iter().all(|note| !note.contains(PLANTED)), "{notes:?}");
    Ok((end, back))
}

#[tokio::test]
async fn a_malformed_request_closes_the_socket_unanswered() -> TestResult {
    let (app, own, aud) = (APPLICATION.as_bytes(), ACCOUNT.as_bytes(), SPACES.as_bytes());
    let planted = PLANTED.as_bytes();
    let mut trailing = request(1, &[app, own, aud]);
    trailing.push(0);
    let mut past_the_end = request(1, &[app, own]);
    past_the_end.extend_from_slice(&100_u32.to_be_bytes());
    past_the_end.extend_from_slice(aud);
    let cases: [(&str, Vec<u8>); 7] = [
        ("an unknown operation", request(9, &[app, own, aud, planted])),
        ("a byte after the last field", trailing),
        ("a missing field", request(1, &[app, own])),
        ("a put without its secret", request(2, &[app, own, aud])),
        ("a get with a secret", request(1, &[app, own, aud, planted])),
        (
            "a field that isn't UTF-8",
            request(2, &[app, own, aud, b"\xff\xfe planted-secret-value-0123456789"]),
        ),
        ("a field longer than the frame", past_the_end),
    ];
    for (case, body) in cases {
        // a well-formed request after it is never read
        let mut bytes = frame(&body);
        bytes.extend(frame(&request(1, &[app, own, aud])));
        let (end, back) = send_raw(bytes, false).await?;
        assert!(matches!(end, Err(ServeError::Malformed)), "{case}: {end:?}");
        assert!(back.is_empty(), "{case}: answered {back:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_frame_length_out_of_range_or_an_end_inside_a_frame_closes_the_socket() -> TestResult {
    let too_long = u32::try_from(MAX_FRAME + 1)?.to_be_bytes().to_vec();
    let (end, back) = send_raw(too_long, false).await?;
    assert!(
        matches!(end, Err(ServeError::BadLength(length)) if length == MAX_FRAME + 1),
        "{end:?}"
    );
    assert!(back.is_empty());

    let (end, back) = send_raw(u32::MAX.to_be_bytes().to_vec(), false).await?;
    assert!(matches!(end, Err(ServeError::BadLength(_))), "{end:?}");
    assert!(back.is_empty());

    let (end, back) = send_raw(0_u32.to_be_bytes().to_vec(), false).await?;
    assert!(matches!(end, Err(ServeError::BadLength(0))), "{end:?}");
    assert!(back.is_empty());

    // half a header, then the end
    let (end, _) = send_raw(vec![0, 0], true).await?;
    assert!(matches!(end, Err(ServeError::Truncated)), "{end:?}");
    // a header promising more than comes
    let mut short = 40_u32.to_be_bytes().to_vec();
    short.extend_from_slice(b"\x01planted-secret-value-0123456789");
    let (end, _) = send_raw(short, true).await?;
    assert!(matches!(end, Err(ServeError::Truncated)), "{end:?}");
    // nothing at all: a clean end
    let (end, back) = send_raw(Vec::new(), true).await?;
    assert!(end.is_ok(), "{end:?}");
    assert!(back.is_empty());
    Ok(())
}

/// A client whose broker answers every request with `answer` (raw bytes), then the broker's end of the socket held
/// open until the returned handle finishes.
fn client_answered_with(answer: Vec<u8>) -> TestResult<(BrokerSecretStore, std::thread::JoinHandle<()>)> {
    let (mut server, client) = UnixStream::pair()?;
    let answering = std::thread::spawn(move || {
        let mut buffer = [0; 1024];
        // one request in, the answer out, then read until the client closes
        if server.read(&mut buffer).is_ok_and(|read| read > 0) {
            let _ = server.write_all(&answer);
        }
        while server.read(&mut buffer).is_ok_and(|read| read > 0) {}
    });
    Ok((BrokerSecretStore::new(client, APPLICATION)?, answering))
}

#[tokio::test]
async fn a_malformed_answer_closes_the_connection_for_every_later_call() -> TestResult {
    let mut too_long = u32::try_from(MAX_FRAME + 1)?.to_be_bytes().to_vec();
    too_long.extend_from_slice(&[0; 16]);
    let cases: [(&str, Vec<u8>); 5] = [
        ("an unknown status", frame(&[9])),
        ("done, to a get", frame(&[0])),
        ("found without its secret", frame(&[1])),
        ("missing with a byte after it", frame(&[2, 0])),
        ("a length over the cap", too_long),
    ];
    for (case, answer) in cases {
        let (client, answering) = client_answered_with(answer)?;
        let error = client.get(&key(ACCOUNT, SPACES)).await.expect_err(case);
        assert!(
            matches!(error, SecretError::Broker(BrokerError::Malformed)),
            "{case}: {error:?}"
        );
        let later = client.get(&key(ACCOUNT, SPACES)).await.expect_err(case);
        assert!(
            matches!(later, SecretError::Broker(BrokerError::Closed)),
            "{case}: {later:?}"
        );
        assert!(format!("{client:?}").contains("open: false"), "{client:?}");
        // the dropped connection closed the socket: the broker's side sees the end
        drop(client);
        answering.join().map_err(|_| "the answering thread panicked")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_broker_gone_fails_the_call_and_every_later_one() -> TestResult {
    let (server, client) = UnixStream::pair()?;
    drop(server);
    let client = BrokerSecretStore::new(client, APPLICATION)?;

    let error = client.get(&key(ACCOUNT, SPACES)).await.expect_err("gone");
    assert!(matches!(error, SecretError::Broker(BrokerError::Io(_))), "{error:?}");
    let error = client.delete(&key(ACCOUNT, SPACES)).await.expect_err("gone");
    assert!(matches!(error, SecretError::Broker(BrokerError::Closed)), "{error:?}");
    Ok(())
}

#[tokio::test]
async fn no_value_reaches_a_debug_an_error_or_a_note() -> TestResult {
    let backing = Arc::new(MemorySecretStore::new());
    let (client, broker) = connect(&backing, APPLICATION)?;
    client.put(&key(ACCOUNT, SPACES), &Secret::new(PLANTED)).await?;
    let got = client.get(&key(ACCOUNT, SPACES)).await?.ok_or("stored")?;
    assert_eq!(got.expose(), PLANTED);
    let refusal = client
        .put(&key(OTHER, SPACES), &Secret::new(PLANTED))
        .await
        .expect_err("refused");
    let too_large = client
        .put(
            &key(ACCOUNT, SPACES),
            &Secret::new(PLANTED.repeat(MAX_FRAME / PLANTED.len() + 1)),
        )
        .await
        .expect_err("too large");

    let mut shown = vec![
        format!("{client:?}"),
        format!("{got:?}"),
        format!("{refusal:?}"),
        refusal.to_string(),
        format!("{too_large:?}"),
        too_large.to_string(),
    ];
    shown.extend(broker.notes());
    for text in &shown {
        assert!(!text.contains(PLANTED), "{text}");
    }
    // a note names the operation, the account and the audience
    assert!(
        shown
            .iter()
            .any(|text| text.contains("put") && text.contains(ACCOUNT) && text.contains(SPACES)),
        "{shown:?}"
    );
    Ok(())
}
