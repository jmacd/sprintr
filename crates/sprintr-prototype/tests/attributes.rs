// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{lines, records, replayed, standalone, template};
use sprintr_prototype::buffer::{RecordBuffer, StackBuffer};
use sprintr_prototype::{
    Definition, Logger, Message, RECORD, Renderer, THREAD, attributes, info, message, replay,
};
use std::io::{self, Write};

attributes! {
    /// Account name
    account_name: str = "account.name";
    /// Retry attempt
    retry_count: u64 = "retry.count";
}

fn log_name<W: Write, B: RecordBuffer>(logger: &mut Logger<W, B>, name: &str) -> io::Result<()> {
    let value = 42u32;
    info!("hello {}, value {value:04x}", logger: logger, account_name = name)
}

/// Scenario: one callsite logs different values, including into an independent stream.
/// Guarantees: definitions accompany first use only; later events carry only IDs and arguments.
#[test]
fn first_record_bootstraps_statement_and_later_records_reuse_it() {
    let mut logger = Logger::new(Vec::new());
    for name in ["Ada", "Grace"] {
        log_name(&mut logger, name).unwrap();
    }
    let encoded = logger.into_inner();
    let startup = &lines(&encoded)[0];
    assert_eq!(
        serde_json::to_value(&startup.defs["log.relation:log.statement"]).unwrap(),
        serde_json::json!({
            "kind": "entity", "type": "object", "desc": "Logging statement", "anonymous": true
        })
    );
    let records = records(&encoded);
    let first = &records[0];
    let id = first.attrs["log.statement"];
    let thread = first.attrs[THREAD];
    let seq = first.attrs[RECORD];
    assert_eq!(
        serde_json::to_value(template(first)).unwrap(),
        serde_json::json!({
            "attrs": {"log.level": "INFO"},
            "refs": [{"account.name": {"$arg": 0}}],
            "msg": "hello {@account.name}, value {value:04x}"
        })
    );
    assert_eq!(
        first.defs.keys().collect::<Vec<_>>(),
        ["log.relation:account.name", &format!("log.statement:{id}")]
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(encoded.split(|&b| b == b'\n').nth(2).unwrap())
            .unwrap(),
        serde_json::json!({
            "seq": seq + 1, "stmt": id, "thread": thread, "args": ["Grace", 42]
        })
    );
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n\
         hello Grace, value 002a\nlog.level = INFO, account.name = Grace\n"
    );

    let mut another = Logger::new(Vec::new());
    log_name(&mut another, "Ada").unwrap();
    let another = common::records(&another.into_inner()).remove(0);
    assert_eq!(another.defs, first.defs);
    assert_eq!(another.args, first.args);
    assert_eq!(another.attrs[THREAD], thread);
    assert!(another.attrs[RECORD] > seq + 1);
}

/// Scenario: two static declarations agree and multiple callsites use the same canonical attribute.
/// Guarantees: aliases never leak into formats, attributes deduplicate, and callsites have distinct identities.
#[test]
fn static_aliases_and_multiple_positions() {
    attributes! {
        /// Account name
        again: str = "account.name";
    }
    use crate::account_name as local_alias;
    let mut logger = Logger::new(Vec::new());
    let before = 7u32;
    info!("{before:x} {} {}", logger: logger, local_alias = "Ada", again = "Grace").unwrap();
    info!("{}", logger: logger, again = "Ada").unwrap();
    let encoded = logger.into_inner();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert_eq!(text.matches("\"log.relation:account.name\"").count(), 1);
    let records = records(&encoded);
    let (first, second) = (&records[0], &records[1]);
    assert_ne!(first.attrs, second.attrs);
    assert_eq!(second.defs.len(), 1);
    assert_eq!(
        serde_json::to_value(&template(first).refs).unwrap(),
        serde_json::json!([
            {"account.name": {"$arg": 1}}, {"account.name": {"$arg": 2}}
        ])
    );
    assert_eq!(
        template(first).msg,
        "{before:x} {@account.name} {@account.name}"
    );
    let owned = message!("{before:x} {} {}", account_name = "Ada", again = "Grace");
    assert_eq!(template(&owned).msg, template(first).msg);
    assert_eq!(template(&owned).refs, template(first).refs);
    assert_eq!(
        owned.render().unwrap(),
        "7 Ada Grace\naccount.name = Ada, account.name = Grace"
    );
}

/// Scenario: an unsigned attribute is formatted in decimal and hexadecimal.
/// Guarantees: untyped placeholders follow the declared type; associations show decimal values.
#[test]
fn unsigned_attributes_render_decimal_and_hex() {
    let mut logger = Logger::new(Vec::new());
    info!("try {} ({:#x})", logger: logger, retry_count = 10u8, retry_count = 255u64).unwrap();
    let encoded = logger.into_inner();
    assert_eq!(
        serde_json::to_value(&records(&encoded)[0].defs["log.relation:retry.count"]).unwrap(),
        serde_json::json!({"kind": "attribute", "type": "u64", "desc": "Retry attempt"})
    );
    assert_eq!(
        template(&records(&encoded)[0]).msg,
        "try {@retry.count} ({@retry.count:#x})"
    );
    assert_eq!(
        replayed(&encoded),
        "try 10 (0xff)\nlog.level = INFO, retry.count = 10, retry.count = 255\n"
    );
}

/// Scenario: an oversized first use is followed by a smaller occurrence of the same statement.
/// Guarantees: encoding rollback preserves all first-use definitions for the retry.
#[test]
fn overflow_does_not_consume_first_definition() {
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<2048>::new());
    assert_eq!(
        log_name(&mut logger, &"x".repeat(2048)).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    log_name(&mut logger, "Ada").unwrap();
    let encoded = logger.into_inner();
    assert_eq!(records(&encoded)[0].defs.len(), 2);
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n"
    );
}

/// Fails only the given write; each record is written with one call.
struct FailNth {
    fail: usize,
    writes: usize,
    output: Vec<u8>,
}

impl Write for FailNth {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.writes - 1 == self.fail {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Scenario: the sink rejects the startup announcement or the first occurrence, then accepts a retry.
/// Guarantees: output failure cannot suppress the announcement or definitions needed by the first accepted event.
#[test]
fn failed_output_does_not_consume_first_definition() {
    for fail in [0, 1] {
        let mut logger = Logger::new(FailNth {
            fail,
            writes: 0,
            output: Vec::new(),
        });
        assert_eq!(
            log_name(&mut logger, "Ada").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        log_name(&mut logger, "Ada").unwrap();
        assert_eq!(
            replayed(&logger.into_inner().output),
            "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n"
        );
    }
}

/// Scenario: wire input contains malformed parameters, unknown relations, or invalid statement shapes.
/// Guarantees: parameters are explicit, bounded, and typed; pointers are not silently reinterpreted.
#[test]
fn playback_rejects_bad_templates() {
    let valid = message!("{}", account_name = "Ada");
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
    missing["stmt"] = serde_json::json!(u64::MAX);
    cases.push(missing);
    let mut zero = base.clone();
    zero["stmt"] = 0.into();
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
    let valid = message!("{}", account_name = "Ada");
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
    attributes! {
        /// A "description"
        escaped: str = "tests.name/~\"\\\n";
    }
    let mut calls = 0;
    let mut logger = Logger::new(Vec::new());
    info!("{}", logger: logger, escaped = {
        calls += 1;
        String::from("Ada\t\"name\"")
    })
    .unwrap();
    assert_eq!(calls, 1);
    let record = standalone(&logger.into_inner());
    assert_eq!(template(&record).msg, format!("{{@{}}}", escaped.name()));
    let Definition::Relation(relation) = &record.defs[&format!("log.relation:{}", escaped.name())]
    else {
        panic!("relation definition")
    };
    assert_eq!(relation.desc, "A \"description\"");
    assert_eq!(
        record.render().unwrap(),
        format!(
            "Ada\t\"name\"\nlog.level = INFO, {} = {}",
            escaped.name().escape_debug(),
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
        info!("{}", logger: logger, account_name = "Ada").unwrap();
        standalone(&logger.into_inner())
    }
    let threads: Vec<_> = (0..4).map(|_| std::thread::spawn(record)).collect();
    let records: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(records.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        records[0].render().unwrap(),
        "Ada\nlog.level = INFO, account.name = Ada"
    );
}
