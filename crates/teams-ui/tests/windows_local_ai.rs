//! Offline npm shim contract: multiline system instructions remain exact and their private file dies with the tool.
#![cfg(all(test, windows))]
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use teams_ui::local_ai::{Backend, Launch, Model, Tool, Work};

#[test]
fn a_claude_batch_shim_reads_multiline_instructions_and_cleans_up() {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("synthetic claude.cmd");
    std::fs::write(
        &program,
        "@echo off\r\npwsh.exe -NoProfile -File \"%~dp0probe.ps1\" %*\r\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("probe.ps1"),
        r"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$arguments = @($args)
$index = [Array]::IndexOf($arguments, '--system-prompt-file')
if ($index -lt 0) { throw 'Missing prompt file' }
$path = $arguments[$index+1]
$text = [IO.File]::ReadAllText($path)
$null = [Console]::ReadLine()
$result = @{path=$path; text=$text} | ConvertTo-Json -Compress
@{type='result'; result=$result} | ConvertTo-Json -Compress
while ($null -ne [Console]::ReadLine()) {}
",
    )
    .unwrap();
    let instructions = "First line & literal %PATH%\r\nSecond line: \"quoted\"\nUnicode: λ";
    let launch = Launch {
        backend: Backend::Claude,
        program,
        work: Work::Summary,
        model: Model::ToolDefault,
        instructions: instructions.to_owned(),
        first: Duration::from_secs(10),
    };
    let stop = AtomicBool::new(false);
    let mut tool = Tool::start(&launch, &stop).unwrap();
    let reply = tool.ask("synthetic input", Duration::from_secs(10), &stop).unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["text"], instructions);
    let path = std::path::PathBuf::from(reply["path"].as_str().unwrap());
    assert!(path.is_file(), "prompt stays available while its tool runs");
    drop(tool);
    assert!(!path.exists(), "prompt is removed after stopping the process tree");
    assert!(!path.parent().unwrap().exists());
}
