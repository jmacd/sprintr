// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{lines, records, replayed, standalone};
use sprintr_prototype::{
    Definition, Logger, Renderer, buffer::StackBuffer, error, info, message, replay, warn,
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
    let startup = serde_json::to_value(&lines(&encoded)[0]).unwrap();
    assert_eq!(
        startup["defs"]["log.relation:log.level"],
        serde_json::json!({
            "kind": "entity", "type": "object", "desc": "Logging level"
        })
    );
    assert_eq!(
        startup["defs"]["log.level:INFO"],
        serde_json::json!({"desc": "Informational event"})
    );
    let records: Vec<serde_json::Value> = records(&encoded)
        .iter()
        .map(|record| serde_json::to_value(record).unwrap())
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
        let id = first["stmt"].as_u64().unwrap();
        let thread = &first["thread"];
        let seq = first["seq"].as_u64().unwrap();
        let template = &first["defs"][format!("log.statement:{id}")];
        assert_eq!(
            template,
            &serde_json::json!({
                "attrs": {"log.level": level}, "refs": [], "msg": "event"
            })
        );
        assert_eq!(
            records[index * 2 + 1],
            serde_json::json!({"seq": seq + 1, "stmt": id, "thread": thread, "args": []})
        );
        assert!(first["defs"].get("log.relation:log.level").is_none());
        if index == 0 {
            assert_eq!(first["defs"].as_object().unwrap().len(), 1);
        } else {
            assert_eq!(
                first["defs"][format!("log.level:{level}")],
                serde_json::json!({"desc": description})
            );
            assert_eq!(first["defs"].as_object().unwrap().len(), 2);
        }
    }
    assert_eq!(records[6]["defs"].as_object().unwrap().len(), 1);
    assert_eq!(
        replayed(&encoded),
        "event\nlog.level = INFO\nevent\nlog.level = INFO\nevent\nlog.level = WARN\n\
         event\nlog.level = WARN\nevent\nlog.level = ERROR\nevent\nlog.level = ERROR\n\
         another\nlog.level = INFO\n"
    );

    let mut independent = Logger::new(Vec::new());
    error_record(&mut independent);
    let first = standalone(&independent.into_inner());
    assert_eq!(first.render().unwrap(), "event\nlog.level = ERROR");
}

/// Scenario: a WARN first-use overflows after INFO has already been accepted.
/// Guarantees: failed encoding does not consume the new named value's definition.
#[test]
fn new_level_definition_survives_overflow() {
    fn emit(logger: &mut Logger<Vec<u8>, StackBuffer<2048>>, text: &str) -> io::Result<()> {
        warn!("{text:s}", logger: logger)
    }
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<2048>::new());
    info!("first", logger: logger).unwrap();
    assert_eq!(
        emit(&mut logger, &"x".repeat(2048)).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    emit(&mut logger, "retry").unwrap();
    let encoded = logger.into_inner();
    let last = &records(&encoded)[1];
    assert!(last.defs.contains_key("log.level:WARN"));
    assert!(!last.defs.contains_key("log.relation:log.level"));
    assert_eq!(
        replayed(&encoded),
        "first\nlog.level = INFO\nretry\nlog.level = WARN\n"
    );
}

/// Scenario: input introduces unknown levels, invalid relations, or malformed named values.
/// Guarantees: missing definitions and unsupported static attributes fail explicitly.
#[test]
fn invalid_level_definitions_and_references_are_rejected() {
    let mut logger = Logger::new(Vec::new());
    info_record(&mut logger);
    let base = serde_json::to_value(standalone(&logger.into_inner())).unwrap();
    let key = format!("log.statement:{}", base["stmt"]);
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
    let valid = standalone(&logger.into_inner());
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
    let Definition::Entity(entity) = conflicting.defs.get_mut("log.level:INFO").unwrap() else {
        unreachable!()
    };
    entity.insert("desc".into(), "Incompatible description".into());
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

/// Scenario: a stream defines its own entity relation with a named value used by a statement.
/// Guarantees: the reader treats every named entity generically, not only `log.level`.
#[test]
fn custom_named_entities_render_generically() {
    let record = serde_json::json!({
        "attrs": {"log.statement": 1},
        "args": [],
        "defs": {
            "log.relation:log.statement": {
                "kind": "entity", "type": "object", "desc": "Logging statement", "anonymous": true
            },
            "log.relation:deployment.environment": {
                "kind": "entity", "type": "object", "desc": "Deployment environment"
            },
            "deployment.environment:PRODUCTION": {"desc": "Production traffic"},
            "log.statement:1": {
                "attrs": {"deployment.environment": "PRODUCTION"}, "refs": [], "msg": "deployed"
            }
        }
    });
    let mut output = Vec::new();
    replay(format!("{record}\n").as_bytes(), &mut output).unwrap();
    assert_eq!(output, b"deployed\ndeployment.environment = PRODUCTION\n");
}
