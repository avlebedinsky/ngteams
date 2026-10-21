-- A conversation's rows that aren't sent (waiting, in flight, denied or failed), for its view: the outbox keeps a row
-- for every write it ever made (each read receipt too), and only these are looked up by conversation.
CREATE INDEX outbox_unsettled_by_conversation ON outbox (conversation_id, id) WHERE state <> 'sent';
