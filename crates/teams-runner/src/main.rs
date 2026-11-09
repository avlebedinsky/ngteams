//! Windows VM runner entry point. Linux uses the existing shell runner and private compositor.
#[cfg(windows)]
mod windows;

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    match windows::run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("ngteams runner: {error}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("use scripts/run-lab.sh on Linux");
        std::process::ExitCode::FAILURE
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn linux_uses_its_existing_shell_runner() {
        assert_eq!(super::main(), std::process::ExitCode::FAILURE);
    }
}
