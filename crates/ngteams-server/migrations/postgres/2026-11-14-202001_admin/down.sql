DELETE FROM server_meta WHERE name IN ('admin_lock', 'setup_code');
DROP TABLE admin_audit;
ALTER TABLE users DROP COLUMN deleted_at;
ALTER TABLE users DROP COLUMN must_change_password;
ALTER TABLE users DROP COLUMN is_admin;
