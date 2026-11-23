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
        json!({"extra":0,"$ref":"#/defs/session:9"}),
        json!([{"$ref":4}]),
        json!([-1]),
        json!({"nested":1.5}),
        json!(-1),
        json!(1.5),
        json!(false),
        Value::Null,
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

fn structured(args: Value, refs: Value) -> Value {
    let mut wire = snapshot();
    wire["args"] = args;
    wire["refs"] = refs;
    let named = json!({"kind":"entity","type":"object","desc":""});
    wire["defs"]["log.relation:struct.name"] = named.clone();
    wire["defs"]["log.relation:struct.encode"] = named;
    wire["defs"]["log.relation:account.name"] =
        json!({"kind":"attribute","type":"string","desc":""});
    wire["defs"]["log.relation:count"] = json!({"kind":"attribute","type":"u64","desc":""});
    wire
}

fn encoder(msg: &str) -> Value {
    json!({"msg":msg,"name":"example::Point","file":"src/main.rs","line":8})
}

/// Scenario: a session is stored literally, or represented by a `$ref` in an
/// older record with a redundant identity annotation.
/// Guarantees: a literal can carry an explicit identity; a `$ref` identifies
/// the same entity with or without a matching annotation.
#[test]
fn literal_identity_and_self_identifying_refs() {
    let literal = structured(json!([{"name":"Ada"}]), json!([{"session:8":"#/args/0"}]));
    let event = message(literal.clone());
    assert_eq!(serde_json::to_value(&event).unwrap(), literal);
    assert!(event.render().unwrap().ends_with(r#"{ name: "Ada" }"#));

    let mut wire = structured(json!([{"$ref":"#/defs/session:9"}]), json!([]));
    let expected = message(wire.clone()).render().unwrap();
    wire["refs"] = json!([{"session:9":"#/args/0"}]);
    assert_eq!(message(wire).render().unwrap(), expected);
}

#[test]
fn entity_attributes_require_refs_to_the_declared_entity_relation() {
    let mut wire = structured(
        json!([[{"$ref":"#/defs/session:9"}]]),
        json!([{"struct.encode:Request_test":"#/args/0"},{"request.session":"#/args/0/0"}]),
    );
    wire["defs"]["log.relation:request.session"] =
        json!({"kind":"attribute","type":{"entity":"session"},"desc":"Handling session"});
    wire["defs"]["struct.encode:Request_test"] = encoder("handling {@request.session}");
    assert!(message(wire.clone()).render().is_ok());
    for wrong in [
        json!("session:9"),
        json!({}),
        json!({"$ref":"#/args/0"}),
        json!({"$ref":"#/defs/log.thread:17"}),
    ] {
        let mut bad = wire.clone();
        bad["args"][0][0] = wrong;
        assert!(message(bad).render().is_err());
    }
    wire["defs"]["log.relation:request.session"]["type"]["entity"] = "count".into();
    assert!(message(wire).render().is_err());
    let field_attribute = structured(
        json!([{"name":"Ada"}]),
        json!([{"struct.name:Account":"#/args/0"},{"account.name":"#/args/0/name"}]),
    );
    assert!(message(field_attribute).render().is_err());
}

#[test]
fn nested_formats_preserve_fields_entity_identity_and_associations() {
    let mut wire = structured(
        json!([{"x":3,"y":4}, ["Ada", 26, ["data", {"$ref":"#/defs/session:9"}], 9], 7]),
        json!([
            {"account.name":"#/args/1/0"},
            {"count":"#/args/1/1"},
            {"struct.name:Point":"#/args/0"},
            {"struct.encode:Outer_abcd":"#/args/1"},
            {"struct.encode:Inner_ef01":"#/args/1/2"}
        ]),
    );
    wire["defs"]["struct.encode:Outer_abcd"] = encoder("{@account.name} {@count:#04x} {child} {n}");
    wire["defs"]["struct.encode:Inner_ef01"] = encoder("{{{text}}}: {entity}");
    wire["defs"]["log.relation:log.level"] = wire["defs"]["log.relation:struct.name"].clone();
    wire["defs"]["log.level:INFO"] = json!({"desc":"Informational"});
    let stmt = &mut wire["defs"]["log.statement:1"];
    stmt["msg"] = "{point} {args} {@count}".into();
    stmt["attrs"] = json!({"log.level":"INFO"});
    stmt["refs"] = json!([{"count":{"$arg":2}}]);
    let event = message(wire.clone());
    assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    assert_eq!(
        event.render().unwrap(),
        "1970-01-01T00:00:00.000Z Point { x: 3, y: 4 } Ada 0x1a {data}: \
         {\"nested\":{\"$ref\":\"#/missing\"}} 9 7\nlog.level = INFO, count = 7, account.name = Ada, count = 26"
    );

    let mut empty = structured(
        json!([[]]),
        json!([{"struct.encode:Empty_1234":"#/args/0"}]),
    );
    empty["defs"]["struct.encode:Empty_1234"] = encoder("");
    assert_eq!(
        message(empty).render().unwrap(),
        "1970-01-01T00:00:00.000Z "
    );
    let bare_root_number = structured(json!([9]), json!([]));
    assert!(message(bare_root_number).render().is_err());
    let plain = structured(json!([["one", 2, {"tag":"three"}]]), json!([]));
    assert_eq!(
        message(plain).render().unwrap(),
        r#"1970-01-01T00:00:00.000Z ["one", 2, { tag: "three" }]"#
    );
}

#[test]
fn pointer_tokens_apply_to_object_keys_and_only_ancestor_edges_are_allowed() {
    let mut path = String::from("#/args/0/");
    push_pointer_token(&mut path, "~/#%é");
    assert_eq!(path, "#/args/0/~0~1%23%25%C3%A9");
    let mut wire = structured(
        json!([{"~/#%é":["Ada", {"$ref": &path}, {"$ref":"#%2Fargs%2F0"}]}]),
        json!([
            {"struct.encode:Node_1234": &path},
            {"account.name":format!("{path}/0")}
        ]),
    );
    wire["defs"]["struct.encode:Node_1234"] = encoder("{@account.name}: {} {}");
    assert_eq!(
        message(wire.clone()).render().unwrap(),
        format!(
            "1970-01-01T00:00:00.000Z {{ ~/#%é: Ada: {{\"$ref\":\"{path}\"}} {{\"$ref\":\"#%2Fargs%2F0\"}} }}\naccount.name = Ada"
        )
    );
    for bad in [
        format!("{path}/0"),  // sibling
        format!("{path}/1"),  // self
        format!("{path}/12"), // nonexistent
        "#/args".into(),
        "0#/args/0".into(),
        "#/args/00".into(),
    ] {
        wire["args"][0]["~/#%é"][1]["$ref"] = bad.as_str().into();
        assert!(message(wire.clone()).render().is_err(), "{bad}");
    }
}

#[test]
fn event_refs_require_single_entries_valid_paths_types_and_templates() {
    for refs in [
        json!(null),
        json!([{}]),
        json!([{"a":"#/args/0","b":"#/args/0"}]),
        json!([{"a":{"$ref":"#/args/0"}}]),
    ] {
        assert!(serde_json::from_value::<Message>(structured(json!([[]]), refs)).is_err());
    }
    let duplicate = r##"{"stmt":1,"ts":0,"attrs":{"log.stack":[]},"args":[],"refs":[{"a":"#/args/0","a":"#/args/1"}]}"##;
    assert!(serde_json::from_str::<Message>(duplicate).is_err());
    for refs in [
        json!([{"unknown":"#/args/0/0"}]),
        json!([{"count":"#/args/0/0"}]),
        json!([{"log.stack":"#/args/0"}]),
        json!([{"account.name":"#/args/0/00"}]),
        json!([{"account.name":"#/args/%GG"}]),
        json!([{"account.name":"#/args/0/~"}]),
        json!([{"account.name":"#/defs/session:9"}]),
        json!([{"account.name":"#/args/0/0/field"}]),
        json!([{"account.name":"1#/args/0/0"}]),
        json!([{"account.name":"#/args/1/0"}]),
        json!([{"struct.name:Point":"#/args/0"}]),
        json!([{"struct.encode:Missing":"#/args/0"}]),
        json!([{"struct.encode:T_1234":"#/args/0/0"}]),
        json!([{"struct.encode:T_1234":"#/args/0"}, {"struct.encode:T_1234":"#%2Fargs/%30"}]),
        json!([{"session:9":"#/args/0/0"}]),
    ] {
        let mut bad = structured(json!([["one"]]), refs.clone());
        bad["defs"]["struct.encode:T_1234"] = encoder("{}");
        assert!(message(bad).render().is_err(), "{refs}");
    }
    for msg in ["{", "{:s}", "{:?}", "no fields", "{:x}", "{@account.name}"] {
        let mut bad = structured(
            json!([["one"]]),
            json!([{"struct.encode:T_1234":"#/args/0"}]),
        );
        bad["defs"]["struct.encode:T_1234"] = encoder(msg);
        assert!(message(bad).render().is_err(), "{msg}");
    }
}

#[test]
fn nested_entity_refs_resolve_against_exact_record_and_do_not_commit_on_failure() {
    let mut renderer = Renderer::default();
    let mut first = addressed(17, 0, "#/defs/session:9");
    first.defs = message(snapshot()).defs;
    renderer.render(&first).unwrap();
    let mut wire = structured(
        json!([[{"$ref":"0#/defs/session:9"}]]),
        json!([{"struct.encode:Session_abcd":"#/args/0"}]),
    );
    wire["defs"]["struct.encode:Session_abcd"] = encoder("session={}");
    wire["seq"] = 1.into();
    wire["thread"] = 17.into();
    wire["defs"].as_object_mut().unwrap().remove("session:9");
    let mut mismatch = wire.clone();
    mismatch["refs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"session:1":"#/args/0/0"}));
    assert!(renderer.render(&message(mismatch)).is_err());
    let mut no_origin = wire.clone();
    no_origin["args"][0][0]["$ref"] = "1#/defs/session:9".into();
    assert!(renderer.render(&message(no_origin)).is_err());
    let mut broken_layout = wire.clone();
    broken_layout["defs"]["struct.encode:Session_abcd"] = encoder("{unclosed");
    assert!(renderer.render(&message(broken_layout)).is_err());
    let mut no_declaration = wire.clone();
    no_declaration["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:struct.encode");
    assert!(renderer.render(&message(no_declaration)).is_err());
    for invalid in [
        json!({"kind":"entity","type":"object","desc":"","anonymous":true}),
        json!({"kind":"attribute","type":"string","desc":""}),
    ] {
        let mut bad = wire.clone();
        bad["defs"]["log.relation:struct.encode"] = invalid;
        assert!(renderer.render(&message(bad)).is_err());
    }
    assert_eq!(
        renderer.render(&message(wire.clone())).unwrap(),
        r##"1970-01-01T00:00:00.000Z session={"nested":{"$ref":"#/missing"}}"##
    );
    wire["seq"] = 2.into();
    wire["defs"] = json!({"struct.encode:Session_abcd": encoder("other={}")});
    assert!(
        renderer
            .render(&message(wire.clone()))
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
    wire["defs"] = json!({});
    assert!(renderer.render(&message(wire)).is_ok());
}
