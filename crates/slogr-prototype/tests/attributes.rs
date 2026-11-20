// SPDX-License-Identifier: Apache-2.0

use slogr_prototype::buffer::StackBuffer;
use slogr_prototype::{Logger, Message, Renderer, info, message, register_attribute, replay};
use std::io::{self, Write};

/// Scenario: a registered alias labels the name argument on the first and subsequent records.
/// Guarantees: the first line bootstraps the relation, refs remain positional, and playback resolves both.
#[test]
fn first_record_bootstraps_attribute_and_later_records_reuse_it() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let name = "Ada";
    let value = 42u32;
    let mut logger = Logger::new(Vec::new());
    info!("hello {}, value {value:04x}", logger: logger, acct_name = name).unwrap();
    info!("hello {}, value {value:04x}", logger: logger, acct_name = name).unwrap();
    let encoded = logger.into_inner();
    let records: Vec<Message> = encoded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(
        serde_json::to_value(&records[0]).unwrap(),
        serde_json::json!({
            "msg": "hello {acct_name}, value {value:04x}",
            "args": ["Ada", 42],
            "defs": {
                "log.relation:account.name": {
                    "kind": "attribute", "type": "string", "desc": "Account name"
                }
            },
            "refs": [{"account.name": "#/args/0"}]
        })
    );
    assert!(records[1].defs.is_empty());
    assert_eq!(records[0].refs, records[1].refs);
    assert_eq!(
        records[0].render().unwrap(),
        "hello Ada, value 002a\naccount.name = Ada"
    );
    let mut output = Vec::new();
    replay(encoded.as_slice(), &mut output).unwrap();
    assert_eq!(
        output,
        b"hello Ada, value 002a\naccount.name = Ada\nhello Ada, value 002a\naccount.name = Ada\n"
    );

    let mut another_stream = Logger::new(Vec::new());
    info!("{}", logger: another_stream, acct_name = name).unwrap();
    let record: Message = serde_json::from_slice(&another_stream.into_inner()).unwrap();
    assert_eq!(record.defs.len(), 1);
}

/// Scenario: identical registrations are reused at multiple argument positions and callsites.
/// Guarantees: one definition suffices, and the refs list preserves every association without overwriting.
#[test]
fn duplicate_registration_and_multiple_positions() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let again = register_attribute("account.name", "Account name").unwrap();
    let before = 7u32;
    let mut logger = Logger::new(Vec::new());
    info!("{before:x} {} {}", logger: logger, acct_name = "Ada", again = "Grace").unwrap();
    info!("{}", logger: logger, again = "Ada").unwrap();
    let encoded = String::from_utf8(logger.into_inner()).unwrap();
    assert_eq!(encoded.matches("\"log.relation:account.name\"").count(), 1);
    let first: Message = serde_json::from_str(encoded.lines().next().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&first.refs).unwrap(),
        serde_json::json!([
            {"account.name": "#/args/1"}, {"account.name": "#/args/2"}
        ])
    );
    assert_eq!(
        first.render().unwrap(),
        "7 Ada Grace\naccount.name = Ada, account.name = Grace"
    );
    let owned = message!("{before:x} {} {}", acct_name = "Ada", again = "Grace");
    assert_eq!(owned, first);
}

/// Scenario: a process attempts incompatible registrations or tries to redefine the bootstrap relation.
/// Guarantees: conflicts are explicit errors and do not replace the original definition.
#[test]
fn registration_conflicts_fail_without_replacing_definitions() {
    let account = register_attribute("tests.conflict", "Original").unwrap();
    let error = register_attribute("tests.conflict", "Different")
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("conflicting"));
    let record = message!("{}", account = "Ada");
    assert_eq!(record.defs["log.relation:tests.conflict"].desc, "Original");
    for name in ["", "log.relation"] {
        assert_eq!(
            register_attribute(name, "Invalid").err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

/// Scenario: a first-use record cannot fit, then a smaller record uses the same attribute.
/// Guarantees: failed encoding does not mark a definition as emitted or leak partial JSON.
#[test]
fn overflow_does_not_consume_first_definition() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<512>::new());
    let large = "x".repeat(1024);
    assert_eq!(
        info!("{}", logger: logger, acct_name = large)
            .unwrap_err()
            .kind(),
        io::ErrorKind::StorageFull
    );
    info!("{}", logger: logger, acct_name = "Ada").unwrap();
    let record: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(record.render().unwrap(), "Ada\naccount.name = Ada");
    assert_eq!(record.defs.len(), 1);
}

struct FailOnce {
    failed: bool,
    output: Vec<u8>,
}

impl Write for FailOnce {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.failed {
            self.failed = true;
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Scenario: the sink rejects the first record before a later retry succeeds.
/// Guarantees: the successfully written record still includes its relation definition.
#[test]
fn failed_output_does_not_consume_first_definition() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let mut logger = Logger::new(FailOnce {
        failed: false,
        output: Vec::new(),
    });
    assert_eq!(
        info!("{}", logger: logger, acct_name = "Ada")
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    info!("{}", logger: logger, acct_name = "Ada").unwrap();
    let record: Message = serde_json::from_slice(&logger.into_inner().output).unwrap();
    assert_eq!(record.render().unwrap(), "Ada\naccount.name = Ada");
}

/// Scenario: a later record repeats a conflicting definition, or references missing/invalid data.
/// Guarantees: playback rejects conflicts and malformed associations with the input line number.
#[test]
fn playback_rejects_bad_relations_and_references() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let first = serde_json::to_value(message!("{}", acct_name = "Ada")).unwrap();
    let mut cases = Vec::new();
    for refs in [
        serde_json::json!({"account.name": "#/args/0"}),
        serde_json::json!([{}]),
        serde_json::json!([{"account.name": "#/args/0", "extra": "#/args/0"}]),
        serde_json::json!([{"account.name": "#/args/1"}]),
        serde_json::json!([{"account.name": "#/args/00"}]),
        serde_json::json!([{"account.name": "#/defs/other"}]),
        serde_json::json!([{"unknown.name": "#/args/0"}]),
    ] {
        let mut bad = first.clone();
        bad["refs"] = refs;
        cases.push(bad);
    }
    let mut conflicting = first.clone();
    conflicting["defs"]["log.relation:account.name"]["desc"] = "Different".into();
    cases.push(conflicting);
    let mut wrong_type = first.clone();
    wrong_type["msg"] = "{value:x}".into();
    wrong_type["args"] = serde_json::json!([42]);
    cases.push(wrong_type);
    let mut unknown_kind = first.clone();
    unknown_kind["defs"]["log.relation:account.name"]["kind"] = "unknown".into();
    cases.push(unknown_kind);
    for bad in cases {
        let input = format!("{first}\n{bad}\n");
        let mut output = Vec::new();
        let error = replay(input.as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 2"), "{error}");
        assert_eq!(output, b"Ada\naccount.name = Ada\n");
    }

    let mut renderer = Renderer::default();
    let valid: Message = serde_json::from_value(first).unwrap();
    renderer.render(&valid).unwrap();
    let mut bad = valid.clone();
    bad.defs.get_mut("log.relation:account.name").unwrap().desc = "Different".into();
    assert!(
        renderer
            .render(&bad)
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
    let mut subsequent = valid;
    subsequent.defs.clear();
    assert_eq!(
        renderer.render(&subsequent).unwrap(),
        "Ada\naccount.name = Ada"
    );
    assert!(
        subsequent
            .render()
            .unwrap_err()
            .to_string()
            .contains("unknown relation")
    );
}

/// Scenario: a relation name and its value contain JSON-sensitive characters.
/// Guarantees: cached encoding preserves names and the association line escapes control characters.
#[test]
fn escaped_names_and_values_roundtrip() {
    let canonical = "tests.name/~\"\\\n";
    let account = register_attribute(canonical, "A \"description\"").unwrap();
    let name = "Ada\t\"name\"";
    let mut logger = Logger::new(Vec::new());
    info!("{}", logger: logger, account = name).unwrap();
    let record: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(record.refs[0][canonical], "#/args/0");
    assert_eq!(
        record.render().unwrap(),
        format!(
            "{name}\n{} = {}",
            canonical.escape_debug(),
            name.escape_debug()
        )
    );
}
