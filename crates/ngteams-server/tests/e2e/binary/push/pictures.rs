//! A message that carries a pasted picture, over the push channel.

use teams_core::engine::{Action, MessageChange};
use teams_proto::chat::{MessageText, Node, RichText};
use teams_proto::{Cloud, PastedFormat, PastedPicture, sent_picture_in};

use super::support::Rig;
use crate::media::png;
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_picture_reaches_the_other_accounts_client_live_and_it_reads_the_bytes() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let cloud = Cloud::own(&rig.origin).unwrap();
        let a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let (chat, _) = super::chat(&a, &mut b, "the chat's first message").await;

        let bytes = png(48, 32);
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: bytes.clone(),
        };
        let object = a.engine().upload_picture(chat.clone(), picture).await.unwrap();
        let image = sent_picture_in(&cloud, &object, (48, 32)).unwrap();
        let src = image.src.clone();
        let text = RichText {
            nodes: vec![Node::Paragraph(vec![Node::Image(image)])],
            truncated: false,
        };
        let sent = a
            .write(Action::Send {
                conversation: chat.clone(),
                text: MessageText::new_in(&cloud, text, []).unwrap(),
            })
            .await;
        let id = sent.message_id.unwrap();

        b.pushed(0, &chat, &id, MessageChange::New).await;
        let message = b.engine().message(&chat, &id).await.unwrap().unwrap();
        assert!(message.content.contains(&src), "{}", message.content);
        let read = b.engine().message_picture(src).await.unwrap().unwrap();
        assert_eq!(read.bytes.as_slice(), bytes.as_slice());
        rig.stop().await;
    }
}
