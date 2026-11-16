DROP TABLE bot_signing_keys;
DROP TABLE app_installs;
DROP TABLE bots;
DROP TABLE app_users;
DROP TABLE apps;
DELETE FROM server_meta WHERE name = 'bot_signing_key_lock';
UPDATE users SET deleted_at = COALESCE(deleted_at, 0), disabled = 1 WHERE kind = 'app';
