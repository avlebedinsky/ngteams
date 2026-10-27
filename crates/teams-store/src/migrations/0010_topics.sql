-- Schema 10: a chat's topic is read from the list (`threadProperties.topic`). The chats stored before it have none,
-- and the list's sync link names only the conversations changed since: the list is read whole again, and every chat
-- without a topic is taken again at whatever version it comes with (its messages' sync goes on from its own link).
UPDATE conversations SET version = 0 WHERE topic IS NULL AND kind NOT IN ('one_to_one', 'channel');
DELETE FROM conversation_list_sync;
