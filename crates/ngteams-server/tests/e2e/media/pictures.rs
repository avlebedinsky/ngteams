//! A picture pasted into a message: made and filled by one engine, read by the chat's other member.

use teams_core::engine::Action;
use teams_proto::chat::{MessageText, Node, RichText};
use teams_proto::{MessagePicture, PastedFormat, PastedPicture, sent_picture_in};
use teams_store::OutboxState;

use super::{Trio, cloud, png, status};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_pastes_a_picture_and_only_the_chats_members_read_it() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let bytes = png(48, 32);
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: bytes.clone(),
        };
        let object = trio.a.upload_picture(trio.chat.clone(), picture).await.unwrap();
        let image = sent_picture_in(&cloud(), &object, (48, 32)).unwrap();
        let src = image.src.clone();
        let text = RichText {
            nodes: vec![Node::Paragraph(vec![Node::Image(image)])],
            truncated: false,
        };
        let row = trio
            .a
            .write(Action::Send {
                conversation: trio.chat.clone(),
                text: MessageText::new_in(&cloud(), text, []).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(trio.b.sync().await.unwrap().failed.is_empty());
        let message = trio
            .b
            .message(&trio.chat, row.message_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(message.content.contains(&src), "{}", message.content);
        let read = trio.b.message_picture(src.clone()).await.unwrap().unwrap();
        assert_eq!(read.bytes.as_slice(), bytes.as_slice());

        let read = MessagePicture::of_in(&cloud(), &src).unwrap();
        let outsider = trio.outsider().await;
        assert_eq!(status(&outsider.read(&read).await.unwrap_err()), Some(403));
        let anonymous = reqwest::Client::new()
            .get(trio.server.url(&format!("/ams/v1/objects/{object}/views/imgo")))
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), 401);
        trio.stop().await;
    }
}
