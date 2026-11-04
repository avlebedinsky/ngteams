//! Reads stay bound to one Core and one environment. Definitions are read before channel association; list summaries
//! alone do not prove a destination. A service continuation is followed only through the typed same-route validator.
use super::*;

pub(super) struct WorkflowPage {
    pub cards: Vec<Card>,
    pub next: Option<String>,
}

pub(super) async fn workflows(
    core: &Arc<dyn Core>,
    environment: &str,
    next: Option<String>,
) -> Result<WorkflowPage, WorkflowFailure> {
    let request = next.map_or_else(
        || WorkflowRequest::List {
            environment: environment.to_owned(),
        },
        |next| WorkflowRequest::Page {
            environment: environment.to_owned(),
            next,
        },
    );
    let answer = core
        .workflow_request(request)
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))?;
    let rows = model::rows(&answer)?;
    let mut cards = Vec::new();
    for chunk in rows.chunks(4) {
        let requests: Vec<_> = chunk
            .iter()
            .map(|row| {
                let id = row
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(WorkflowFailure::InvalidResponse)?;
                Ok((
                    id.to_owned(),
                    core.workflow_request(WorkflowRequest::Get {
                        environment: environment.to_owned(),
                        flow: id.to_owned(),
                    }),
                ))
            })
            .collect::<Result<_, _>>()?;
        let reads = requests.into_iter().map(async |(id, reply)| {
            let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection))?;
            let card = Card::from_flow(&answer).ok_or(WorkflowFailure::InvalidResponse)?;
            if card.id != id {
                return Err(WorkflowFailure::InvalidResponse);
            }
            Ok(card)
        });
        cards.extend(
            futures_util::future::join_all(reads)
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(WorkflowPage {
        cards,
        next: model::next(&answer),
    })
}
