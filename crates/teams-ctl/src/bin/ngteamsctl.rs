//! `ngteamsctl`: drives the running `ngteams` over its control socket ([`teams_ctl::cli`]).

fn main() -> std::process::ExitCode {
    teams_ctl::cli::main()
}
