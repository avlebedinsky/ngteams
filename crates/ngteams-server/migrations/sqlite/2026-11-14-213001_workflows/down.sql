DROP TABLE workflow_runs;
DROP TABLE workflow_owners;
DROP TABLE workflows;
DROP TABLE workflow_connections;
DELETE FROM users WHERE id = '358f0194-6b0e-4dd3-af35-c24fe8a9ec87';
ALTER TABLE users DROP COLUMN kind;
