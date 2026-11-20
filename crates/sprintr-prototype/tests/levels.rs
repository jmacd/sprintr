// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::{
    Definition, Logger, Message, Renderer, buffer::StackBuffer, error, info, message, replay, warn,
};
use std::io;

fn info_record(logger: &mut Logger<Vec<u8>>) {
    info!("event", logger: logger).unwrap();
}

fn warn_record(logger: &mut Logger<Vec<u8>>) {
    warn!("event", logger: logger).unwrap();
}

fn error_record(logger: &mut Logger<Vec<u8>>) {
    error!("event", logger: logger).unwrap();
}

/// Scenario: three level macros are used twice each and INFO is used at another callsite.
/// Guarantees: levels are named entities with descriptions, static in templates, and emitted once per stream.
#[test]
fn named_levels_are_static_and_deduplicated() {
    let mut logger = Logger::new(Vec::new());
    for emit in [info_record, warn_record, error_record] {
        emit(&mut logger);
        emit(&mut logger);
    }
    info!("another", logger: logger).unwrap();
    let encoded = logger.into_inner();
    let records: Vec<serde_json::Value> = encoded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    for (index, (level, description)) in [
        ("INFO", "Informational event"),
        ("WARN", "Warning condition"),
        ("ERROR", "Error condition"),
    ]
    .into_iter()
    .enumerate()
    {
        let first = &records[index * 2];
        let id = first["attrs"]["log.statement"].as_u64().unwrap();
        let template = &first["defs"][format!("log.statement:{id}")];
        assert_eq!(
            template,
            &serde_json::json!({
                "attrs": {"log.level": level}, "refs": [], "msg": "event"
            })
        );
        assert_eq!(
            first["defs"][format!("log.level:{level}")],
            serde_json::json!({"desc": description})
        );
        assert_eq!(
            records[index * 2 + 1],
            serde_json::json!({"attrs": {"log.statement": id}, "args": []})
        );
        if index != 0 {
            assert!(first["defs"].get("log.relation:log.level").is_none());
            assert_eq!(first["defs"].as_object().unwrap().len(), 2);
        }
    }
    assert_eq!(
        records[0]["defs"]["log.relation:log.level"],
        serde_json::json!({
            "kind": "entity", "type": "object", "desc": "Logging level"
        })
    );
    assert_eq!(records[6]["defs"].as_object().unwrap().len(), 1);
    let mut output = Vec::new();
    replay(encoded.as_slice(), &mut output).unwrap();
    assert_eq!(output, b"event\nlog.level = INFO\nevent\nlog.level = INFO\nevent\nlog.level = WARN\nevent\nlog.level = WARN\nevent\nlog.level = ERROR\nevent\nlog.level = ERROR\nanother\nlog.level = INFO\n");

    let mut independent = Logger::new(Vec::new());
    error_record(&mut independent);
    let first: Message = serde_json::from_slice(&independent.into_inner()).unwrap();
    assert_eq!(first.render().unwrap(), "event\nlog.level = ERROR");
}

/// Scenario: a WARN first-use overflows after INFO has already been accepted.
/// Guarantees: failed encoding does not consume the new named value's definition.
#[test]
fn new_level_definition_survives_overflow() {
    fn emit(logger: &mut Logger<Vec<u8>, StackBuffer<1024>>, text: &str) -> io::Result<()> {
        warn!("{text:s}", logger: logger)
    }
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<1024>::new());
    info!("first", logger: logger).unwrap();
    assert_eq!(
        emit(&mut logger, &"x".repeat(2048)).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    emit(&mut logger, "retry").unwrap();
    let encoded = logger.into_inner();
    let last: Message =
        serde_json::from_slice(encoded.split(|byte| *byte == b'\n').nth(1).unwrap()).unwrap();
    assert!(last.defs.contains_key("log.level:WARN"));
    assert!(!last.defs.contains_key("log.relation:log.level"));
    let mut output = Vec::new();
    replay(encoded.as_slice(), &mut output).unwrap();
    assert_eq!(
        output,
        b"first\nlog.level = INFO\nretry\nlog.level = WARN\n"
    );
}

/// Scenario: input introduces unknown levels, invalid relations, or malformed named values.
/// Guarantees: missing definitions and unsupported static attributes fail explicitly.
#[test]
fn invalid_level_definitions_and_references_are_rejected() {
    let mut logger = Logger::new(Vec::new());
    info_record(&mut logger);
    let base: serde_json::Value = serde_json::from_slice(&logger.into_inner()).unwrap();
    let key = format!("log.statement:{}", base["attrs"]["log.statement"]);
    let mut cases = Vec::new();
    for attrs in [
        serde_json::json!({"log.level": "MISSING"}),
        serde_json::json!({"log.level": 1}),
        serde_json::json!({"other": "INFO"}),
    ] {
        let mut bad = base.clone();
        bad["defs"][&key]["attrs"] = attrs;
        cases.push(bad);
    }
    for missing in ["log.relation:log.level", "log.level:INFO"] {
        let mut bad = base.clone();
        bad["defs"].as_object_mut().unwrap().remove(missing);
        cases.push(bad);
    }
    let mut anonymous = base.clone();
    anonymous["defs"]["log.relation:log.level"]["anonymous"] = true.into();
    cases.push(anonymous);
    let mut empty_name = base.clone();
    empty_name["defs"]["log.level:"] = serde_json::json!({"desc": "Invalid"});
    cases.push(empty_name);
    let mut bad_shape = base;
    bad_shape["defs"]["log.level:INFO"] = serde_json::json!({"desc": "Extra", "args": []});
    cases.push(bad_shape);
    let first = serde_json::to_string(&message!("first")).unwrap();
    for bad in cases {
        let mut output = Vec::new();
        let error = replay(format!("{first}\n{bad}\n").as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("line 2"), "{error}");
        assert_eq!(output, b"first\n");
    }
}

/// Scenario: invalid occurrences or conflicting descriptions arrive with level definitions.
/// Guarantees: only accepted records extend the reader's level table; equal repeats remain valid.
#[test]
fn named_level_definitions_are_transactional_and_immutable() {
    let mut logger = Logger::new(Vec::new());
    info_record(&mut logger);
    let valid: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    let mut invalid = valid.clone();
    invalid
        .args
        .push(sprintr_prototype::Argument::String("unexpected".into()));
    let mut renderer = Renderer::default();
    assert!(renderer.render(&invalid).is_err());
    let mut missing_level = valid.clone();
    missing_level.defs.remove("log.level:INFO");
    assert!(
        renderer
            .render(&missing_level)
            .unwrap_err()
            .to_string()
            .contains("unknown entity")
    );
    assert_eq!(renderer.render(&valid).unwrap(), "event\nlog.level = INFO");
    assert_eq!(renderer.render(&valid).unwrap(), "event\nlog.level = INFO");
    let mut conflicting = valid.clone();
    let Definition::NamedEntity(entity) = conflicting.defs.get_mut("log.level:INFO").unwrap()
    else {
        unreachable!()
    };
    entity.desc = "Incompatible description".into();
    assert!(
        renderer
            .render(&conflicting)
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
    let mut subsequent = valid;
    subsequent.defs.clear();
    assert_eq!(
        renderer.render(&subsequent).unwrap(),
        "event\nlog.level = INFO"
    );
}
