-- What the directory knows of an account beside its name: the person card's fields, who it reports to, whether its
-- automatic replies are on, and its picture in the file store. Times are Unix seconds.
CREATE TABLE user_profiles (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    given_name TEXT,
    surname TEXT,
    job_title TEXT,
    department TEXT,
    office_location TEXT,
    company_name TEXT,
    mobile_phone TEXT,
    business_phone TEXT,
    manager_id TEXT REFERENCES users (id) ON DELETE SET NULL,
    out_of_office BOOLEAN NOT NULL DEFAULT FALSE,
    picture_key TEXT,
    picture_size BIGINT NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL
);
CREATE INDEX user_profiles_manager ON user_profiles (manager_id);
