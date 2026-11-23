// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{FailNth, lines, records, replayed, standalone, started_logger, template};
use sprintr_prototype::buffer::{RecordBuffer, StackBuffer};
use sprintr_prototype::{
    Definition, Logger, Message, RECORD, Renderer, THREAD, attributes, info, replay,
};
use std::io::{self, Write};

attributes! {
    /// Account name
    account_name: str = "account.name";
    /// Retry attempt
    retry_count: u64 = "retry.count";
}

fn log_name<W: Write, B: RecordBuffer>(logger: &mut Logger<W, B>, name: &str) {
    let value = 42u32;
    logger.with_default(|| info!("hello {}, value {value:04x}", account_name = name))
}

/// Scenario: one callsite logs different values, including into an independent stream.
/// Guarantees: definitions accompany first use only; later events carry only IDs and arguments.
#[test]
fn first_record_bootstraps_statement_and_later_records_reuse_it() {
    let mut logger = started_logger();
    for name in ["Ada", "Grace"] {
        log_name(&mut logger, name);
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
    let pcs = &first.stack;
    assert!(pcs.len() <= sprintr_prototype::DEFAULT_STACK_DEPTH && !pcs.contains(&0));
    assert!(startup.defs.contains_key("log.relation:log.source"));
    assert!(startup.defs.contains_key("log.relation:log.stack"));
    assert_eq!(
        serde_json::to_value(template(first)).unwrap(),
        serde_json::json!({
            "attrs": {"log.level": "INFO"},
            "refs": [{"account.name": {"$arg": 0}}],
            "msg": "hello {@account.name}, value {value:04x}",
            "source": template(first).source
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
            "seq": seq + 1, "stmt": id, "thread": thread, "ts": records[1].timestamp, "attrs": {"log.stack": records[1].stack}, "args": ["Grace", 42]
        })
    );
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n\
         hello Grace, value 002a\nlog.level = INFO, account.name = Grace\n"
    );

    let mut another = started_logger();
    log_name(&mut another, "Ada");
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
    let mut logger = started_logger();
    let before = 7u32;
    logger.with_default(|| info!("{before:x} {} {}", local_alias = "Ada", again = "Grace"));
    logger.with_default(|| info!("{}", again = "Ada"));
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
    assert_eq!(
        replayed(&encoded),
        "7 Ada Grace\nlog.level = INFO, account.name = Ada, account.name = Grace\n\
         Ada\nlog.level = INFO, account.name = Ada\n"
    );
}

/// Scenario: an unsigned attribute is formatted in decimal and hexadecimal.
/// Guarantees: untyped placeholders follow the declared type; associations show decimal values.
#[test]
fn unsigned_attributes_render_decimal_and_hex() {
    let mut logger = started_logger();
    logger.with_default(|| info!("try {} ({:#x})", retry_count = 10u8, retry_count = 255u64));
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
    logger.start().unwrap();
    log_name(&mut logger, &"x".repeat(2048));
    log_name(&mut logger, "Ada");
    let encoded = logger.into_inner();
    assert_eq!(records(&encoded)[0].defs.len(), 2);
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n"
    );
}

/// The first accepted occurrence still carries its definitions after a rejected write.
#[test]
fn failed_output_does_not_consume_first_definition() {
    let mut logger = Logger::new(FailNth::new(2));
    logger.start().unwrap();
    log_name(&mut logger, "rejected");
    log_name(&mut logger, "Ada");
    let sink = logger.into_inner();
    assert_eq!(sink.writes, 3);
    assert_eq!(
        replayed(&sink.output),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada\n"
    );
}

/// If even the startup record failed, the first accepted event must replay on its own.
#[test]
fn rejected_announcement_does_not_consume_first_definition() {
    let mut logger = Logger::new(FailNth::new(1));
    assert_eq!(
        logger.start().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    log_name(&mut logger, "Ada");
    let sink = logger.into_inner();
    assert_eq!(sink.writes, 2);
    let events = lines(&sink.output);
    assert_eq!(events.len(), 1);
    assert_eq!(
        common::body(&events[0].render().unwrap()),
        "hello Ada, value 002a\nlog.level = INFO, account.name = Ada"
    );
}

/// Scenario: wire input contains malformed parameters, unknown relations, or invalid statement shapes.
/// Guarantees: parameters are explicit, bounded, and typed; pointers are not silently reinterpreted.
#[test]
fn playback_rejects_bad_templates() {
    let mut logger = started_logger();
    logger.with_default(|| info!("{}", account_name = "Ada"));
    let events = lines(&logger.into_inner());
    let first = serde_json::to_string(&events[0]).unwrap();
    let valid = &events[1];
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
    for bad in cases {
        let mut output = Vec::new();
        let error = replay(format!("{first}\n{bad}\n").as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 2"), "{error}");
        assert_eq!(
            common::body(std::str::from_utf8(&output).unwrap()),
            common::startup_text()
        );
    }
}

/// Scenario: invalid first-use records or conflicting repeats reach an existing reader.
/// Guarantees: failed records do not install definitions or corrupt previously compiled templates.
#[test]
fn renderer_updates_are_transactional() {
    let mut logger = started_logger();
    logger.with_default(|| info!("{}", account_name = "Ada"));
    let valid = standalone(&logger.into_inner());
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
            common::body(&renderer.render(&subsequent).unwrap()),
            "Ada\nlog.level = INFO, account.name = Ada"
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
    let mut logger = started_logger();
    logger.with_default(|| {
        info!(
            "{}",
            escaped = {
                calls += 1;
                String::from("Ada\t\"name\"")
            }
        )
    });
    assert_eq!(calls, 1);
    let record = standalone(&logger.into_inner());
    assert_eq!(template(&record).msg, format!("{{@{}}}", escaped.name()));
    let Definition::Relation(relation) = &record.defs[&format!("log.relation:{}", escaped.name())]
    else {
        panic!("relation definition")
    };
    assert_eq!(relation.desc, "A \"description\"");
    assert_eq!(
        common::body(&record.render().unwrap()),
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
        let mut logger = started_logger();
        logger.with_default(|| info!("{}", account_name = "Ada"));
        standalone(&logger.into_inner())
    }
    let threads: Vec<_> = (0..4).map(|_| std::thread::spawn(record)).collect();
    let records: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].attrs == pair[1].attrs && pair[0].defs == pair[1].defs)
    );
    assert_eq!(
        common::body(&records[0].render().unwrap()),
        "Ada\nlog.level = INFO, account.name = Ada"
    );
}
