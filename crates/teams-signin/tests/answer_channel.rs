//! The answer channel on its own (`harness = false`: a test harness prints to stdout itself): the test runs itself
//! again as a child, which takes stdout for the answer ([`cli::take_stdout`]), then prints to stdout, has a spawned
//! shell print to it, and writes the answer through the file it was given. The child's stdout must hold the answer
//! line and nothing else; what it and its shell printed must have gone to stderr.

use std::io::Write as _;
use std::process::{Command, ExitCode, Stdio};

use teams_auth::signin::WindowAnswer;
use teams_signin::cli;

const CHILD_ENV: &str = "TEAMS_SIGNIN_ANSWER_CHANNEL_CHILD";
const NOISE: &str = "noise printed to stdout after it was taken";
const SHELL_NOISE: &str = "noise a child process printed to stdout";
const REASON: &str = "a made-up failure";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let outcome = if std::env::var_os(CHILD_ENV).is_some() {
        child()
    } else {
        parent()
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("answer channel test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn child() -> TestResult {
    let mut out = cli::take_stdout()?;
    println!("{NOISE}");
    std::io::stdout().flush()?;
    let status = Command::new("sh")
        .args(["-c", &format!("echo '{SHELL_NOISE}'")])
        .status()?;
    if !status.success() {
        return Err(format!("the shell exited {status}").into());
    }
    cli::write_answer(&mut out, &WindowAnswer::Failed(REASON.into()))?;
    Ok(())
}

fn parent() -> TestResult {
    let output = Command::new(std::env::current_exe()?)
        .env(CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    if !output.status.success() {
        return Err(format!("the child exited {}: {stderr}", output.status).into());
    }
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() != 1 {
        return Err(format!("one answer line and nothing else on stdout: {stdout:?}").into());
    }
    let WindowAnswer::Failed(reason) = WindowAnswer::parse(lines[0])? else {
        return Err(format!("not the child's answer: {stdout:?}").into());
    };
    if reason != REASON {
        return Err(format!("not the child's reason: {reason}").into());
    }
    for noise in [NOISE, SHELL_NOISE] {
        if !stderr.contains(noise) {
            return Err(format!("{noise:?} isn't on stderr: {stderr:?}").into());
        }
    }
    println!("answer channel: ok");
    Ok(())
}
