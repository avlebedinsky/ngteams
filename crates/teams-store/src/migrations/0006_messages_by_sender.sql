-- A person's name as their messages last carried it (`imdisplayname`), whatever the conversation: the chat list names
-- a 1:1 chat by the other member and a group chat without a topic by its members, each looked up by MRI.
CREATE INDEX messages_by_sender ON messages (sender, arrival_time DESC) WHERE sender_name IS NOT NULL;
