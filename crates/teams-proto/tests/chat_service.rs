//! Chat-service base/host/path recognition retained independently of the removed membership source.

use teams_proto::chat_service_path;
use url::Url;

#[test]
fn the_recognized_service_routes_decode_the_same_request_path() {
    let path = "v1/users/ME/conversations/19%3Aa_b%40thread.v2/messages";
    let expected = ["users", "ME", "conversations", "19:a_b@thread.v2", "messages"]
        .map(String::from)
        .to_vec();
    for base in [
        "https://teams.microsoft.com/api/chatsvc/emea/",
        "https://teams.cloud.microsoft/api/chatsvc/amer/",
        "https://emea.ng.msg.teams.microsoft.com/",
    ] {
        let url = Url::parse(&format!("{base}{path}")).unwrap();
        assert_eq!(chat_service_path(&url), Some(expected.clone()), "{url}");
    }
}

#[test]
fn urls_outside_the_service_routes_or_with_invalid_decoding_are_not_chat_paths() {
    let path = "v1/users/ME/conversations";
    for base in [
        "http://teams.microsoft.com/api/chatsvc/emea/",
        "https://teams.microsoft.com:444/api/chatsvc/emea/",
        "https://user:pw@teams.microsoft.com/api/chatsvc/emea/",
        "https://teams.microsoft.com/api/mt/emea/",
        "https://msg.teams.microsoft.com/",
        "https://emea.ng.msg.teams.microsoft.com/api/",
        "https://chatsvcagg.teams.microsoft.com/",
        "https://graph.microsoft.com/v1.0/chats/",
        "https://teams.microsoft.com/api/chatsvc/EMEA/",
        "https://teams.microsoft.com/api/chatsvc/x%20y/",
        "https://teams.microsoft.com/api/chatsvc//",
        "https://a_b.msg.teams.microsoft.com/",
        "https://a..b.msg.teams.microsoft.com/",
        "https://teams.microsoft.com.example.com/api/chatsvc/emea/",
    ] {
        let url = Url::parse(&format!("{base}{path}")).unwrap();
        assert_eq!(chat_service_path(&url), None, "{url}");
    }
    for url in [
        "https://teams.microsoft.com/api/chatsvc/emea/v2/users",
        "https://teams.microsoft.com/api/chatsvc/emea/v1/%FF",
        "data:text/plain,not-a-service",
    ] {
        assert_eq!(chat_service_path(&Url::parse(url).unwrap()), None, "{url}");
    }
}
