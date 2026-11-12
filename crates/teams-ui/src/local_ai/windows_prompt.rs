//! Rust deliberately rejects newlines in cmd/bat arguments. Claude's documented file form preserves the exact
//! system prompt through npm shims without hand-written shell quoting. Only the private file path crosses cmd.

use super::{Backend, Launch};
use std::io::{self, Write as _};
use teams_platform::private::{self, Open};

pub(super) fn prepare(launch: &Launch, mut args: Vec<String>) -> io::Result<(Vec<String>, Option<tempfile::TempDir>)> {
    let batch = launch
        .program
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"));
    if launch.backend != Backend::Claude || !batch {
        return Ok((args, None));
    }
    let directory = tempfile::Builder::new().prefix("ngteams-ai-prompt-").tempdir()?;
    private::restrict_dir(directory.path())?;
    let path = directory.path().join("instructions.txt");
    let mut file = private::open(&path, Open::NewReadWrite)?;
    file.write_all(launch.instructions.as_bytes())?;
    drop(file);
    let index = args
        .iter()
        .position(|arg| arg == "--system-prompt")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Claude system prompt argument is missing"))?;
    "--system-prompt-file".clone_into(&mut args[index]);
    args[index + 1] = path.to_string_lossy().into_owned();
    Ok((args, Some(directory)))
}
