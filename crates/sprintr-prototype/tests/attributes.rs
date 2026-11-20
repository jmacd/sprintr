// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::buffer::{RecordBuffer, StackBuffer};
use sprintr_prototype::{
    Attribute, Definition, Logger, Message, Renderer, StatementTemplate, attribute, info, message,
    replay,
};
use std::io::{self, Write};

static ACCT_NAME: Attribute = attribute!("account.name", "Account name");

fn template(record: &Message) -> &StatementTemplate {
    let key = format!("log.statement:{}", record.attrs["log.statement"]);
    let Definition::Statement(template) = &record.defs[&key] else {
        panic!("statement definition")
    };
    template
}

fn log_name<W: Write, B: RecordBuffer>(logger: &mut Logger<W, B>, name: &str) -> io::Result<()> {
    let value = 42u32;
    info!("hello {}, value {value:04x}", logger: logger, ACCT_NAME = name)
}

/// Scenario: one callsite logs different values, including into an independent stream.
/// Guarantees: definitions accompany first use only; later events contain only attrs and args.
#[test]
fn first_record_bootstraps_statement_and_later_records_reuse_it() {
    let mut logger = Logger::new(Vec::new());
    for name in ["Ada", "Grace"] {
        log_name(&mut logger, name).unwrap();
    }
    let encoded = logger.into_inner();
    let records: Vec<serde_json::Value> = encoded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let first: Message = serde_json::from_value(records[0].clone()).unwrap();
    let id = first.attrs["log.statement"];
    assert_eq!(
        serde_json::to_value(template(&first)).unwrap(),
        serde_json::json!({
            "attrs": {},
            "refs": [{"account.name": {"$arg": 0}}],
            "msg": "hello {@account.name}, value {value:04x}"
        })
    );
    assert_eq!(
        records[0]["defs"]["log.relation:log.statement"],
        serde_json::json!({
            "kind": "entity", "type": "object", "desc": "Logging statement", "anonymous": true
        })
    );
    assert_eq!(
        records[1],
        serde_json::json!({
            "attrs": {"log.statement": id}, "args": ["Grace", 42]
        })
    );
    let mut output = Vec::new();
    replay(encoded.as_slice(), &mut output).unwrap();
    assert_eq!(output, b"hello Ada, value 002a\naccount.name = Ada\nhello Grace, value 002a\naccount.name = Grace\n");

    let mut another = Logger::new(Vec::new());
    log_name(&mut another, "Ada").unwrap();
    let record: Message = serde_json::from_slice(&another.into_inner()).unwrap();
    assert_eq!(record, first);
}

/// Scenario: two static declarations agree and multiple callsites use the same canonical attribute.
/// Guarantees: aliases never leak into formats, attributes deduplicate, and callsites have distinct identities.
#[test]
fn static_aliases_and_multiple_positions() {
    static AGAIN: Attribute = attribute!("account.name", "Account name");
    use crate::ACCT_NAME as local_alias;
    let mut logger = Logger::new(Vec::new());
    let before = 7u32;
    info!("{before:x} {} {}", logger: logger, local_alias = "Ada", AGAIN = "Grace").unwrap();
    info!("{}", logger: logger, AGAIN = "Ada").unwrap();
    let encoded = String::from_utf8(logger.into_inner()).unwrap();
    assert_eq!(encoded.matches("\"log.relation:account.name\"").count(), 1);
    let mut records = encoded
        .lines()
        .map(|line| serde_json::from_str::<Message>(line).unwrap());
    let first = records.next().unwrap();
    let second = records.next().unwrap();
    assert_ne!(first.attrs, second.attrs);
    assert_eq!(second.defs.len(), 1);
    assert_eq!(
        serde_json::to_value(&template(&first).refs).unwrap(),
        serde_json::json!([
            {"account.name": {"$arg": 1}}, {"account.name": {"$arg": 2}}
        ])
    );
    assert_eq!(
        template(&first).msg,
        "{before:x} {@account.name} {@account.name}"
    );
    let owned = message!("{before:x} {} {}", ACCT_NAME = "Ada", AGAIN = "Grace");
    assert_eq!(template(&owned), template(&first));
    assert_eq!(
        owned.render().unwrap(),
        "7 Ada Grace\naccount.name = Ada, account.name = Grace"
    );
}

/// Scenario: separately compiled static declarations disagree about a process-wide name.
/// Guarantees: first use rejects the conflicting schema without writing an event.
#[test]
fn static_conflicts_fail_before_output() {
    static ORIGINAL: Attribute = attribute!("tests.conflict", "Original");
    static CONFLICT: Attribute = attribute!("tests.conflict", "Different");
    let mut logger = Logger::new(Vec::new());
    info!("{}", logger: logger, ORIGINAL = "Ada").unwrap();
    assert_eq!(
        info!("{}", logger: logger, CONFLICT = "Ada")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let record: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(record.render().unwrap(), "Ada\ntests.conflict = Ada");
}

/// Scenario: an oversized first use is followed by a smaller occurrence of the same statement.
/// Guarantees: encoding rollback preserves all first-use definitions for the retry.
#[test]
fn overflow_does_not_consume_first_definition() {
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<512>::new());
    assert_eq!(
        log_name(&mut logger, &"x".repeat(1024)).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    log_name(&mut logger, "Ada").unwrap();
    let record: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(record.defs.len(), 3);
    assert_eq!(
        record.render().unwrap(),
        "hello Ada, value 002a\naccount.name = Ada"
    );
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

/// Scenario: the sink rejects the first occurrence and accepts a retry.
/// Guarantees: output failure cannot suppress definitions needed by the first accepted event.
#[test]
fn failed_output_does_not_consume_first_definition() {
    let mut logger = Logger::new(FailOnce {
        failed: false,
        output: Vec::new(),
    });
    assert_eq!(
        log_name(&mut logger, "Ada").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    log_name(&mut logger, "Ada").unwrap();
    let record: Message = serde_json::from_slice(&logger.into_inner().output).unwrap();
    assert_eq!(
        record.render().unwrap(),
        "hello Ada, value 002a\naccount.name = Ada"
    );
}

/// Scenario: wire input contains malformed parameters, unknown relations, or invalid statement shapes.
/// Guarantees: parameters are explicit, bounded, and typed; pointers are not silently reinterpreted.
#[test]
fn playback_rejects_bad_templates() {
    let valid = message!("{}", ACCT_NAME = "Ada");
    let key = format!("log.statement:{}", valid.attrs["log.statement"]);
    let base = serde_json::to_value(valid).unwrap();
    let mut cases = Vec::new();
    for refs in [
        serde_json::json!([{}]),
        serde_json::json!([{"account.name": {"$arg": 0}, "other": {"$arg": 0}}]),
        serde_json::json!([{"account.name": {"$arg": 1}}]),
        serde_json::json!([{"account.name": {"$arg": -1}}]),
        serde_json::json!([{"account.name": {"$arg": 0, "extra": 1}}]),
        serde_json::json!([{"account.name": "#/args/0"}]),
        serde_json::json!([{"unknown.name": {"$arg": 0}}]),
        serde_json::json!([]),
    ] {
        let mut bad = base.clone();
        bad["defs"][&key]["refs"] = refs;
        cases.push(bad);
    }
    let mut wrong_type = base.clone();
    wrong_type["defs"][&key]["msg"] = "{value:x}".into();
    wrong_type["args"] = serde_json::json!([42]);
    cases.push(wrong_type);
    for field in ["args", "defs"] {
        let mut bad = base.clone();
        bad["defs"][&key][field] = serde_json::json!([]);
        cases.push(bad);
    }
    let mut missing = base.clone();
    missing["attrs"]["log.statement"] = serde_json::json!(u64::MAX);
    cases.push(missing);
    let mut zero = base.clone();
    zero["attrs"]["log.statement"] = 0.into();
    cases.push(zero);
    let first = serde_json::to_string(&message!("first")).unwrap();
    for bad in cases {
        let mut output = Vec::new();
        let error = replay(format!("{first}\n{bad}\n").as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 2"), "{error}");
        assert_eq!(output, b"first\n");
    }
}

/// Scenario: invalid first-use records or conflicting repeats reach an existing reader.
/// Guarantees: failed records do not install definitions or corrupt previously compiled templates.
#[test]
fn renderer_updates_are_transactional() {
    let valid = message!("{}", ACCT_NAME = "Ada");
    let mut subsequent = valid.clone();
    subsequent.defs.clear();
    let mut renderer = Renderer::default();
    let mut invalid = valid.clone();
    invalid.args.clear();
    assert!(renderer.render(&invalid).is_err());
    assert!(renderer.render(&subsequent).is_err());
    renderer.render(&valid).unwrap();
    for change_statement in [false, true] {
        let mut invalid = valid.clone();
        if change_statement {
            let key = format!("log.statement:{}", valid.attrs["log.statement"]);
            let Definition::Statement(template) = invalid.defs.get_mut(&key).unwrap() else {
                unreachable!()
            };
            template.msg = "different {@account.name}".into();
        } else {
            let Definition::Relation(relation) =
                invalid.defs.get_mut("log.relation:account.name").unwrap()
            else {
                unreachable!()
            };
            relation.desc = "Different".into();
        }
        assert!(
            renderer
                .render(&invalid)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
        assert_eq!(
            renderer.render(&subsequent).unwrap(),
            "Ada\naccount.name = Ada"
        );
    }
}

/// Scenario: static canonical names and temporary argument values need JSON escaping.
/// Guarantees: pre-encoded templates roundtrip and value expressions execute once without ownership transfer.
#[test]
fn escaped_names_and_temporary_values_roundtrip() {
    static ESCAPED: Attribute = attribute!("tests.name/~\"\\\n", "A \"description\"");
    let mut calls = 0;
    let mut logger = Logger::new(Vec::new());
    info!("{}", logger: logger, ESCAPED = {
        calls += 1;
        String::from("Ada\t\"name\"")
    })
    .unwrap();
    assert_eq!(calls, 1);
    let record: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(template(&record).msg, format!("{{@{}}}", ESCAPED.name()));
    assert_eq!(
        record.render().unwrap(),
        format!(
            "Ada\t\"name\"\n{} = {}",
            ESCAPED.name().escape_debug(),
            "Ada\t\"name\"".escape_debug()
        )
    );
}

/// Scenario: one static callsite is first used concurrently by independent output streams.
/// Guarantees: its identity and template are stable, and every stream bootstraps itself.
#[test]
fn concurrent_first_use_has_one_identity() {
    fn record() -> Message {
        let mut logger = Logger::new(Vec::new());
        info!("{}", logger: logger, ACCT_NAME = "Ada").unwrap();
        serde_json::from_slice(&logger.into_inner()).unwrap()
    }
    let threads: Vec<_> = (0..4).map(|_| std::thread::spawn(record)).collect();
    let records: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(records.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(records[0].render().unwrap(), "Ada\naccount.name = Ada");
}
