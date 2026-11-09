//! The sign-in engine's download window, drawn in the UI sandbox: while downloading and after a failure. It runs itself
//! again inside the sandbox; the shots land in `target/ui-shots/` for a person to read.

use std::process::ExitCode;
use std::time::Duration;

use gpui::{App, WindowingModes};
use teams_signin::engine::Preview;
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "TEAMS_ENGINE_WAIT_TEST";
const APP_ID: &str = "com.nickalie.ngteams.signin";
const WAIT: Duration = Duration::from_secs(30);
const STEP: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    if std::env::var_os(CHILD_ENV).is_some() {
        child();
        return ExitCode::SUCCESS;
    }
    match parent() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("engine wait test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn child() {
    gpui_platform::linux(WindowingModes::WAYLAND).run(|cx: &mut App| {
        let preview = Preview::open(APP_ID, cx);
        preview.progress(42_000_000, 118_000_000);
        cx.spawn(async move |cx| {
            cx.background_executor().timer(STEP).await;
            say("progress");
            cx.background_executor().timer(STEP).await;
            preview.failed("The web engine download is truncated (41000000 of 118000000 bytes).");
            cx.background_executor().timer(STEP).await;
            say("failed");
            cx.background_executor().timer(STEP).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("engine wait window", &SandboxOptions::default())? else {
        return Ok(());
    };
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "1")])?;
    let mut running = Running::new(child)?;
    drive(&sandbox, &mut running).map_err(|error| running.fail(&error))?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    say("engine wait window: ok");
    Ok(())
}

fn drive(sandbox: &Sandbox, running: &mut Running) -> SandboxResult {
    running
        .line_where(WAIT, |line| line == "progress")
        .map_err(|error| format!("no progress: {error:?}"))?;
    keep(sandbox, "engine-wait-progress.png")?;
    running
        .line_where(WAIT, |line| line == "failed")
        .map_err(|error| format!("no failure: {error:?}"))?;
    keep(sandbox, "engine-wait-failed.png")?;
    Ok(())
}

/// Screenshots the window into `target/ui-shots/`, where a person reads it.
fn keep(sandbox: &Sandbox, name: &str) -> SandboxResult {
    let shot = sandbox.screenshot(name, true)?;
    let shots = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots");
    std::fs::create_dir_all(&shots)?;
    std::fs::copy(&shot, shots.join(name))?;
    Ok(())
}
