// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::{Logger, info, replay};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tempfile::NamedTempFile;

fn recording() -> Vec<u8> {
    let mut logger = Logger::new(Vec::new());
    logger.start().unwrap();
    logger.with_default(|| {
        for name in ["Ada", "Grace"] {
            info!("hello {name:s}");
        }
    });
    logger.into_inner()
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sprintr"))
}

fn pipe(args: &[&str], input: &[u8]) -> Output {
    let mut child = cli()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// Scenario: a producer reuses a statement in a live stream or saved file.
/// Guarantees: both input modes share the library's stateful text rendering.
#[test]
fn stdin_and_saved_file_match_library_playback() {
    let input = recording();
    let mut expected = Vec::new();
    replay(&input[..], &mut expected).unwrap();
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&input).unwrap();
    for output in [
        pipe(&["replay"], &input),
        pipe(&["replay", "-"], &input),
        cli().arg("replay").arg(file.path()).output().unwrap(),
    ] {
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, expected);
        assert!(output.stderr.is_empty());
    }
}

/// Scenario: bad input follows already accepted records.
/// Guarantees: the valid prefix is printed and a line-numbered error goes to stderr.
#[test]
fn malformed_line_reports_failure_after_valid_output() {
    let mut input = recording();
    let next = input.iter().filter(|&&b| b == b'\n').count() + 1;
    input.extend_from_slice(b"not a record\n");
    let output = pipe(&["replay"], &input);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello Grace"));
    assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("line {next}:")));
}

/// Scenario: a user chooses the recorded-symbol or sidecar stack display.
/// Guarantees: the CLI honors the choice and rejects conflicting sources.
#[test]
fn selects_one_stack_source() {
    let mut events: Vec<sprintr_prototype::Message> =
        serde_json::Deserializer::from_slice(&recording())
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap();
    events.last_mut().unwrap().stack = vec![4097];
    let mut input = Vec::new();
    for event in events {
        serde_json::to_writer(&mut input, &event).unwrap();
        input.push(b'\n');
    }
    let mut map = NamedTempFile::new().unwrap();
    map.write_all(br#"{"modules":[{"path":"app","debug_id":null,"segments":[{"loaded":4096,"linked":256,"size":16}]}]}"#).unwrap();
    let path = map.path().to_str().unwrap();
    let with_map = pipe(&["replay", "--module-map", path], &input);
    assert!(with_map.status.success());
    assert!(String::from_utf8_lossy(&with_map.stdout).contains("at 0x1001 (app @ 0x100)"));
    let embedded = pipe(&["replay", "--symbols"], &input);
    assert!(embedded.status.success());
    assert!(String::from_utf8_lossy(&embedded.stdout).contains("at <unmapped> (0x1001)"));
    let conflict = cli()
        .args(["replay", "--symbols", "--module-map", path])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
}
