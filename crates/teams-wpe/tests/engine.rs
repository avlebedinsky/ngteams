//! Native WPE hosts and workers against generated pages in a disposable UI sandbox.

#[cfg(test)]
#[path = "support/engine/mod.rs"]
mod harness;

fn main() {
    // SAFETY: first operation in main, while this process has one thread.
    unsafe { teams_wpe::worker::serve_if_asked(|| {}) };
    let code = match harness::entry() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("WPE engine test: {error}");
            1
        }
    };
    if std::env::var_os(harness::CHILD).is_some() {
        teams_wpe::exit_process(code);
    }
    std::process::exit(code);
}
