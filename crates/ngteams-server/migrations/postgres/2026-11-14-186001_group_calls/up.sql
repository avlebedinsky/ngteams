ALTER TABLE calls ADD COLUMN group_chat TEXT;
CREATE UNIQUE INDEX calls_running_in_group_chat ON calls (group_chat) WHERE group_chat IS NOT NULL AND phase <> 'ended';
