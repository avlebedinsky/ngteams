//! The lab config, `~/.config/teams-lab/lab.toml`: the 3 lab accounts, the approved service participants and the lab
//! directories. It lives in `teams-fence`, because the fence takes its roster and its log path from it
//! ([`teams_fence::Fence::open_lab`]); the harness uses it from here.

pub use teams_fence::lab::{
    ACCOUNT_COUNT, Account, Alias, ConfigError, LabConfig, LabLocations, LabPaths, MriRecord, ServiceParticipant,
    record_account_mri, record_service_mri,
};
