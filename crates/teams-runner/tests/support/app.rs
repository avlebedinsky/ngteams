// Synthetic process fixture. No GUI, network or credentials. Built by rustc only for runner lifecycle tests.
use std::{fs::File, io::Write, os::windows::io::FromRawHandle, time::Duration};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("build") { return; }
    if args.get(1).map(String::as_str) == Some("--worker") {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    if args.get(1).map(String::as_str) == Some("snapshot") {
        std::fs::create_dir_all(&args[3]).unwrap();
        std::fs::write(std::path::Path::new(&args[3]).join("synthetic"), "snapshot").unwrap();
        if std::env::current_exe().unwrap().file_name().unwrap() == "snapshot-fails.exe" { std::process::exit(3); }
        return;
    }
    if args.get(1).map(String::as_str) == Some("secrets") {
        if args[3] == "broker-exits" { std::process::exit(3); }
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let index = args.iter().position(|arg| arg == "--trace-fd").unwrap() + 1;
    // SAFETY: this synthetic process was given this fresh, uniquely transferred writable trace handle.
    let mut trace = unsafe { File::from_raw_handle(args[index].parse::<usize>().unwrap() as _) };
    trace.write_all(b"synthetic app started\n").unwrap();
    if args[2] == "app-exits" { std::process::exit(7); }
    if args[2] == "restart" {
        let _worker = std::process::Command::new(std::env::current_exe().unwrap()).arg("--worker").spawn().unwrap();
        return;
    }
    std::thread::sleep(Duration::from_secs(30));
}
