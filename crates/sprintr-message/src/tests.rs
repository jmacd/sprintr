// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde_json::json;

fn snapshot() -> Value {
    let relation = json!({"kind":"entity","type":"object","desc":"","anonymous":true});
    json!({
        "stmt":1,
        "ts":0,
        "attrs":{"log.stack":[42]},
        "args":[{"$ref":"#/defs/session:9"}],
        "defs":{
            "log.relation:log.statement":relation,
            "log.relation:log.stack":{"kind":"attribute","type":"[u64]","desc":"Stack"},
            "log.relation:log.source":relation,
            "log.relation:log.thread":relation,
            "log.relation:log.record":relation,
            "log.relation:session":relation,
            "log.statement:1":{"attrs":{},"refs":[],"msg":"{}","source":{"mod":"app","file":"src/main.rs","line":12,"column":5}},
            "log.thread:17":{"name":"main"},
            "session:9":{"nested":{"$ref":"#/missing"}}
        }
    })
}

fn message(value: Value) -> Message {
    serde_json::from_value(value).unwrap()
}

#[test]
fn stack_pcs_use_a_source_schema_not_individual_definitions() {
    let mut wire = snapshot();
    wire["attrs"]["log.stack"] = json!([u64::MAX, 42, 42]);
    assert!(message(wire.clone()).render().is_ok());
    for bad in [Value::Null, json!("42"), json!([0]), json!([-1])] {
        let mut bad_wire = wire.clone();
        bad_wire["attrs"]["log.stack"] = bad;
        if let Ok(event) = serde_json::from_value::<Message>(bad_wire) {
            assert!(event.render().is_err());
        }
    }
    wire["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:log.source");
    assert!(message(wire).render().is_err());
}

#[test]
fn timestamp_retains_nanoseconds_and_displays_milliseconds() {
    let mut wire = snapshot();
    wire["ts"] = json!(1_704_067_200_123_999_999_u64);
    let record = message(wire.clone());
    assert_eq!(serde_json::to_value(&record).unwrap(), wire);
    assert!(
        record
            .render()
            .unwrap()
            .starts_with("2024-01-01T00:00:00.123Z ")
    );

    for invalid in [json!(-1), json!(1.5), json!("123"), Value::Null] {
        wire["ts"] = invalid;
        assert!(serde_json::from_value::<Message>(wire.clone()).is_err());
    }
    wire.as_object_mut().unwrap().remove("ts");
    assert!(serde_json::from_value::<Message>(wire).is_err());
}

fn addressed(thread: u64, sequence: u64, target: &str) -> Message {
    message(json!({
        "seq":sequence,"stmt":1,"thread":thread,"ts":0,
        "attrs":{"log.stack":[42]},
        "args":[{"$ref":target}]
    }))
}

#[test]
fn entity_objects_roundtrip_without_shape_dispatch_or_traversal() {
    let key = "a/~%#é:9";
    let fragment = definition_fragment(key);
    assert_eq!(fragment, "#/defs/a~1~0%25%23%C3%A9:9");
    let mut snapshot = snapshot();
    snapshot["defs"]["log.relation:a/~%#é"] = snapshot["defs"]["log.relation:session"].clone();
    snapshot["args"][0]["$ref"] = fragment.into();
    for object in [
        json!({"attrs":{},"refs":[],"msg":"{invalid"}),
        json!({"kind":"attribute","type":"u64","desc":"not a declaration"}),
        json!({"$ref":"#/missing"}),
        json!({}),
    ] {
        snapshot["defs"][key] = object.clone();
        let record = message(snapshot.clone());
        assert!(matches!(record.defs[key], Definition::Entity(_)));
        assert_eq!(serde_json::to_value(&record).unwrap(), snapshot);
        assert_eq!(
            record.render().unwrap(),
            format!("1970-01-01T00:00:00.000Z {object}")
        );
    }
}

#[test]
fn entity_and_address_validation_uses_the_declared_relation() {
    let attrs = json!({"log.thread":17,"log.record":3,"log.stack":[42]});
    for (key, value) in [
        ("log.thread:17", Value::Null),
        ("session:9", json!([])),
        ("session:abc", json!({})),
        ("log.record:3", json!({})),
        (
            "log.relation:log.thread",
            json!({"kind":"entity","type":"object","desc":""}),
        ),
        (
            "log.relation:log.record",
            json!({"kind":"attribute","type":"u64","desc":""}),
        ),
    ] {
        let mut record = snapshot();
        record["attrs"] = attrs.clone();
        record["defs"][key] = value;
        if let Ok(record) = serde_json::from_value::<Message>(record) {
            assert!(record.render().is_err(), "{key}");
        }
    }
    for missing in ["log.thread:17", "log.relation:log.record"] {
        let mut record = snapshot();
        record["attrs"] = attrs.clone();
        record["defs"].as_object_mut().unwrap().remove(missing);
        assert!(message(record).render().is_err(), "{missing}");
    }
}

#[test]
fn shorthand_expands_to_canonical_attributes_without_overrides() {
    let mut compact = snapshot();
    compact["seq"] = 0.into();
    compact["thread"] = 17.into();
    let mut longhand = compact.clone();
    longhand.as_object_mut().unwrap().remove("seq");
    longhand.as_object_mut().unwrap().remove("stmt");
    longhand.as_object_mut().unwrap().remove("thread");
    longhand["attrs"] = json!({"log.record":0,"log.statement":1,"log.thread":17,"log.stack":[42]});
    let event = message(longhand);
    assert_eq!(event, message(compact.clone()));
    assert_eq!(event.attrs[RECORD], 0);
    assert_eq!(event.attrs[STATEMENT], 1);
    assert_eq!(serde_json::to_value(&event).unwrap(), compact);
    assert!(event.render().is_ok());

    for (field, name) in [("seq", RECORD), ("stmt", STATEMENT), ("thread", THREAD)] {
        let mut both = compact.clone();
        both["attrs"][name] = compact[field].clone();
        assert!(serde_json::from_value::<Message>(both).is_err());
        let mut null = compact.clone();
        null[field] = Value::Null;
        assert!(serde_json::from_value::<Message>(null).is_err());
    }
}

#[test]
fn references_require_the_definition_in_the_selected_record_on_this_thread() {
    let mut renderer = Renderer::default();
    let mut first = addressed(17, 0, "0#/defs/session:9");
    first.defs = message(snapshot()).defs;
    let text = r##"1970-01-01T00:00:00.000Z {"nested":{"$ref":"#/missing"}}"##;
    assert_eq!(renderer.render(&first).unwrap(), text);

    // Both threads accept record 0, but only thread 17 defined the session in it.
    let mut other = addressed(20, 0, "#/defs/log.thread:20");
    other
        .defs
        .insert("log.thread:20".into(), Definition::Entity(Map::new()));
    assert_eq!(
        renderer.render(&other).unwrap(),
        "1970-01-01T00:00:00.000Z {}"
    );
    assert!(
        renderer
            .render(&addressed(20, 4, "0#/defs/session:9"))
            .is_err()
    );
    assert_eq!(
        renderer
            .render(&addressed(17, 5, "0#/defs/session:9"))
            .unwrap(),
        text
    );

    // Record 5 used the key but did not define it. Nor does the current record.
    for target in ["5#/defs/session:9", "#/defs/session:9", "8#/defs/session:9"] {
        assert!(
            renderer.render(&addressed(17, 8, target)).is_err(),
            "{target}"
        );
    }
    // An equal repeat is an addressable definition in its own right.
    let mut repeated = addressed(17, 8, "#/defs/session:9");
    repeated
        .defs
        .insert("session:9".into(), first.defs["session:9"].clone());
    assert_eq!(renderer.render(&repeated).unwrap(), text);
    assert_eq!(
        renderer
            .render(&addressed(17, 10, "8#/defs/session:9"))
            .unwrap(),
        text
    );
    assert!(
        renderer
            .render(&addressed(17, 10, "0#/defs/session:9"))
            .is_err()
    );
    assert!(
        renderer
            .render(&addressed(17, 9, "0#/defs/session:9"))
            .is_err()
    );
}

#[test]
fn failed_records_install_neither_definitions_nor_addresses() {
    let mut renderer = Renderer::default();
    let mut first = addressed(17, 1, "#/defs/log.thread:17");
    first.defs = message(snapshot()).defs;
    renderer.render(&first).unwrap();
    let mut bad = addressed(17, 42, "#/defs/missing:1");
    bad.defs
        .insert("session:10".into(), Definition::Entity(Map::new()));
    assert!(renderer.render(&bad).is_err());

    let mut next = addressed(17, 2, "#/defs/session:10");
    next.defs
        .insert("session:10".into(), first.defs["session:9"].clone());
    renderer.render(&next).unwrap(); // neither an out-of-order record nor a conflict
    assert!(
        renderer
            .render(&addressed(17, 43, "42#/defs/session:10"))
            .is_err()
    );
    let mut conflict = addressed(17, 43, "#/defs/session:10");
    conflict.defs = bad.defs;
    assert!(renderer.render(&conflict).is_err());
    assert!(
        renderer
            .render(&addressed(17, 44, "43#/defs/session:10"))
            .is_err()
    );
    renderer
        .render(&addressed(17, 43, "2#/defs/session:10"))
        .unwrap();
}

#[test]
fn malformed_addresses_and_incompatible_arguments_fail() {
    let mut record = message(snapshot());
    for target in [
        "1#/defs/session:9", // unaddressed snapshot
        "#/args/0",
        "#/defs/session:9/nested",
        "#/defs/session:9%2Fnested",
        "#/defs/session:9%",
        "#/defs/session:9%XX",
        "#/defs/session:9%FF",
        "#/defs/session:9~",
        "#/defs/session:9~2",
        "#/defs/session:9#",
        "00#/defs/session:9",
        "+1#/defs/session:9",
        "01#/defs/session:9",
        "#/defs/log.statement:1",
        "#/defs/log.relation:session",
    ] {
        record.args[0] = Argument::Reference {
            target: target.into(),
        };
        assert!(record.render().is_err(), "{target}");
    }
    for arg in [
        json!({"$ref":"#/defs/session:9","extra":0}),
        json!(["#/defs/session:9"]),
    ] {
        assert!(serde_json::from_value::<Argument>(arg).is_err());
    }

    let mut base = snapshot();
    base["defs"]["log.statement:1"]["msg"] = "{:x}".into();
    assert!(message(base.clone()).render().is_err());
    base["defs"]["log.relation:attr"] = json!({"kind":"attribute","type":"string","desc":""});
    base["defs"]["log.statement:1"]["refs"] = json!([{"attr":{"$arg":0}}]);
    base["defs"]["log.statement:1"]["msg"] = "{}".into();
    assert!(message(base).render().is_err());
}
