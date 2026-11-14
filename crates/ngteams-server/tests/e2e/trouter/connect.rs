//! The socket's life: the connect, the authentication, `trouter.connected`, keep-alives and the disconnect.

use teams_trouter::PushEvent;

use super::{Account, ORIGIN, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn the_clients_session_connects_authenticates_and_learns_its_endpoint() {
    for backend in backends() {
        let server = server(backend).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let account = Account::sign_in(&server, "a@own.test", &id).await;
        let trouter = account.trouter(&server);
        let opened = trouter.opened_urls();
        let mut push = super::Push(trouter.spawn());
        let (endpoint, new_endpoint, push_url) = push
            .until(|event| match event {
                PushEvent::Connected {
                    endpoint,
                    new_endpoint,
                    push_url,
                } => Some((endpoint, new_endpoint, push_url)),
                _ => None,
            })
            .await;
        assert!(new_endpoint);
        assert_eq!(push_url, format!("{ORIGIN}/trouter/v4/f/{endpoint}/"));
        assert_eq!(endpoint.len(), 22);
        assert!(
            endpoint
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert_eq!(opened.urls()[0].path(), "/trouter/v4/c");
        push.stop().await;
        server.stop().await;
    }
}
