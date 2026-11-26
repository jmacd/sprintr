// SPDX-License-Identifier: Apache-2.0

use super::*;

const ROOT: &str = "log.thread:17/span.start:0";
const CHILD: &str = "log.thread:17/span.start:2";
const GRANDCHILD: &str = "log.thread:20/span.start:0";
const TRACE: u128 = 0xffaa_0000_0000_0000_0000_0000_0000_001a;

fn links(span: u64, current: &str, root: &str, parent: Option<(u64, &str)>) -> Value {
    let mut refs = vec![
        json!({format!("trace.start:{TRACE:032x}"):root}),
        json!({format!("span.start:{span:016x}"):current}),
    ];
    if let Some((id, address)) = parent {
        refs.push(json!({format!("span.parent:{id:016x}"):address}));
    }
    Value::Array(refs)
}

fn event(stmt: u64, thread: u64, seq: u64, refs: Value) -> Value {
    let args = if stmt == 3 {
        json!([])
    } else {
        json!(["name"])
    };
    json!({"ts":0,"thread":thread,"seq":seq,"stmt":stmt,"attrs":{"log.stack":[]},"args":args,"refs":refs})
}

fn with_schema(mut wire: Value) -> Value {
    let mut defs = snapshot()["defs"].take();
    let global = json!({"kind":"entity","type":"object","desc":"","id_scope":"global"});
    let local = json!({"kind":"entity","type":"object","desc":"","id_scope":"local",
        "scope_entity_type":TRACE_START,"id_keys":["span.id"]});
    defs["log.relation:span.start"] = local.clone();
    defs["log.relation:span.parent"] = local;
    defs["log.relation:trace.start"] = global.clone();
    defs["log.relation:trace.start"]["id_keys"] = json!(["trace.id"]);
    for key in ["span.id", "trace.id"] {
        defs[format!("log.relation:{key}")] = json!({"kind":"attribute","type":"string","desc":""});
    }
    defs["log.relation:log.kind"] = global;
    defs["log.thread:20"] = json!({});
    for (id, kind, text) in [
        (2, "SPAN_START", "{}"),
        (3, "SPAN_END", "span ended"),
        (4, "CONTEXT_START", "{}"),
    ] {
        let mut stmt = defs["log.statement:1"].clone();
        stmt["msg"] = json!(text);
        stmt["attrs"] = json!({"log.kind":kind});
        defs[format!("log.statement:{id}")] = stmt;
        defs[format!("log.kind:{kind}")] = json!({"desc":""});
    }
    wire["defs"] = defs;
    wire
}

#[test]
fn start_parentage_membership_and_end_survive_replay() {
    let mut reader = Renderer::default();
    let mut root = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    let parent_schema = root["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:span.parent")
        .unwrap();
    let root_event = message(root.clone());
    assert_eq!(serde_json::to_value(&root_event).unwrap(), root);
    assert_eq!(
        reader.render(&root_event).unwrap(),
        "1970-01-01T00:00:00.000Z name\nlog.kind = SPAN_START"
    );
    let root = root_event.span_links().unwrap().unwrap();
    assert_eq!(root.root.id.get(), TRACE);
    assert_eq!(root.current.id.get(), 10);
    assert_eq!(root.root.address, root.current.address);
    assert_eq!(root.parent, None);

    let mut child = event(2, 17, 2, links(11, CHILD, ROOT, Some((10, ROOT))));
    child["defs"] = json!({"log.relation:span.parent":parent_schema});
    reader.render(&message(child)).unwrap();
    let grandchild = message(event(
        2,
        20,
        0,
        links(12, GRANDCHILD, ROOT, Some((11, CHILD))),
    ));
    reader.render(&grandchild).unwrap();
    let grandchild = grandchild.span_links().unwrap().unwrap();
    let parent = reader
        .span_start(grandchild.parent.unwrap().address)
        .unwrap();
    assert_eq!(grandchild.parent, Some(parent.current));
    assert_eq!(parent.parent, Some(root.current));
    assert_eq!(grandchild.root, root.root);

    for (seq, stmt, text) in [
        (1, 1, "name"),
        (2, 4, "name\nlog.kind = CONTEXT_START"),
        (3, 3, "span ended\nlog.kind = SPAN_END"),
    ] {
        let member = message(event(stmt, 20, seq, links(12, GRANDCHILD, ROOT, None)));
        assert_eq!(
            reader.render(&member).unwrap(),
            format!("1970-01-01T00:00:00.000Z {text}")
        );
        let links = member.span_links().unwrap().unwrap();
        assert_eq!(links.parent, None);
        assert_eq!(reader.span_start(links.current.address), Some(grandchild));
    }
    let duplicate_end = message(event(3, 20, 4, links(12, GRANDCHILD, ROOT, None)));
    assert!(reader.render(&duplicate_end).is_err());
}

#[test]
fn span_ids_can_repeat_across_traces_but_not_at_two_addresses_within_one_trace() {
    let mut reader = Renderer::default();
    let first = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    reader.render(&message(first)).unwrap();
    let mut second_refs = links(10, GRANDCHILD, GRANDCHILD, None);
    second_refs[0] = json!({format!("trace.start:{:032x}", TRACE + 1):GRANDCHILD});
    reader
        .render(&message(event(2, 20, 0, second_refs)))
        .unwrap();

    // The target is an otherwise legal gap in this trace. The second root
    // must not have overwritten the first trace's reverse lookup.
    let gap = "log.thread:17/span.start:1";
    assert!(
        reader
            .render(&message(event(1, 17, 2, links(10, gap, ROOT, None))))
            .is_err()
    );
    reader
        .render(&message(event(1, 17, 2, links(10, ROOT, ROOT, None))))
        .unwrap();
}

#[test]
fn explicit_span_scopes_must_agree_with_marker_semantics() {
    let mut reader = Renderer::default();
    let root = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    for (name, scope, parent) in [
        (SPAN_START, "global", None),
        (SPAN_PARENT, "local", Some("log.kind")),
        (TRACE_START, "local", Some("log.kind")),
    ] {
        let mut bad = root.clone();
        let def = &mut bad["defs"][format!("log.relation:{name}")];
        def["id_scope"] = scope.into();
        if let Some(parent) = parent {
            def["scope_entity_type"] = parent.into();
        } else {
            def.as_object_mut().unwrap().remove("scope_entity_type");
        }
        assert!(reader.render(&message(bad)).is_err(), "{name}");
        assert!(reader.relation(name).is_none());
    }

    for (name, field, value) in [
        (TRACE_START, "id_keys", json!(["span.id"])),
        (SPAN_START, "id_keys", json!(["trace.id"])),
        (SPAN_PARENT, "id_keys", json!(["trace.id", "span.id"])),
        ("span.id", "type", json!("u64")),
    ] {
        let mut bad = root.clone();
        bad["defs"][format!("log.relation:{name}")][field] = value;
        assert!(reader.render(&message(bad)).is_err(), "{name}");
    }
    let mut missing_key_schema = root.clone();
    missing_key_schema["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:trace.id");
    assert!(reader.render(&message(missing_key_schema)).is_err());
    let mut legacy = root;
    for name in [SPAN_START, TRACE_START, SPAN_PARENT] {
        let def = legacy["defs"][format!("log.relation:{name}")]
            .as_object_mut()
            .unwrap();
        def.remove("id_scope");
        def.remove("scope_entity_type");
        def.remove("id_keys");
    }
    let defs = legacy["defs"].as_object_mut().unwrap();
    defs.remove("log.relation:span.id");
    defs.remove("log.relation:trace.id");
    reader.render(&message(legacy)).unwrap();
    assert_eq!(reader.relation(TRACE_START).unwrap().id_scope, None);
}

#[test]
fn marker_as_a_scope_carries_both_fixed_width_ids_without_marker_definitions() {
    let mut reader = Renderer::default();
    let mut wire = with_schema(event(1, 17, 0, json!([])));
    wire["defs"]["log.relation:step.number"] = json!({"kind":"attribute","type":"u64","desc":""});
    wire["defs"]["log.relation:step"] = json!({"kind":"entity","type":"object","desc":"",
        "id_scope":"local","scope_entity_type":SPAN_START,"id_keys":["step.number"]});
    let key = format!(r#"step:[["{TRACE:032x}"],["000000000000000a"],[3]]"#);
    wire["defs"][&key] = json!({"description":"child"});
    reader.render(&message(wire)).unwrap();
    assert_eq!(reader.identity_components(&key).unwrap().len(), 3);
    for bad in [
        key.replace("000000000000000a", "a"),
        key.replace(&format!("{TRACE:032x}"), "0"),
    ] {
        assert!(reader.identity_components(&bad).is_err());
    }
}

#[test]
fn span_ids_and_process_addresses_are_canonical_and_not_pointers() {
    let max = RecordAddress::new(u64::MAX, u64::MAX).unwrap();
    assert_eq!(
        RecordAddress::parse_span_start_ref(&max.span_start_ref().to_string()).unwrap(),
        max
    );
    let mut wire = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    for target in [
        "#/args/0",
        "#/defs/span.start:0",
        "#log.thread:17/span.start:0",
        "log.thread:0/span.start:0",
        "log.thread:017/span.start:0",
        "log.thread:+17/span.start:0",
        "log.thread:17/span.start:00",
        "log.thread:17/span.start:+0",
        "log.thread:17/span.start:%30",
        "log.thread:17/span.start:18446744073709551616",
        "log.thread:17/span.start:0#",
        "log.thread:17/log.record:0",
    ] {
        wire["refs"][1] = json!({"span.start:000000000000000a":target});
        assert!(message(wire.clone()).render().is_err(), "{target}");
    }
    for key in [
        "span.start",
        "span.start:0",
        "span.start:0000000000000000",
        "span.start:000000000000000A",
        "span.start:+00000000000000a",
        "span.start:0000000000000000a",
        "span.start:000000000000000g",
        "trace.start:00000000000000000000000000000000",
        "trace.start:000000000000000a",
        "span.parent:0",
    ] {
        wire["refs"][1] = json!({key: ROOT});
        assert!(message(wire.clone()).span_links().is_err(), "{key}");
    }
}

#[test]
fn relations_and_lifecycle_markers_cannot_disagree_with_the_refs() {
    let mut reader = Renderer::default();
    let root = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    for name in [SPAN_START, TRACE_START, SPAN_PARENT] {
        for bad in [
            json!({"kind":"entity","type":"object","anonymous":true,"desc":""}),
            json!({"kind":"attribute","type":"string","desc":""}),
        ] {
            let mut wire = root.clone();
            wire["defs"][format!("log.relation:{name}")] = bad;
            assert!(reader.render(&message(wire)).is_err(), "{name}");
        }
        let mut wire = root.clone();
        wire["defs"][format!("{name}:000000000000000a")] = json!({"desc":""});
        assert!(reader.render(&message(wire)).is_err(), "{name}");
    }
    let mut missing_schema = root.clone();
    missing_schema["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:span.start");
    assert!(reader.render(&message(missing_schema)).is_err());
    let valid = root["refs"].clone();
    for (stmt, seq, refs) in [
        (2, 0, json!([])),
        (3, 1, json!([])),
        (1, 1, json!([valid[0]])),
        (2, 0, json!([valid[0], valid[1], valid[1]])),
        (2, 0, links(10, CHILD, CHILD, None)), // must name this start
        (2, 0, links(10, ROOT, ROOT, Some((11, CHILD)))), // root cannot have a parent
        (2, 2, links(11, CHILD, ROOT, None)),  // child needs a parent
        (2, 2, links(11, CHILD, ROOT, Some((10, CHILD)))), // child's own address
        (2, 2, links(11, CHILD, ROOT, Some((11, ROOT)))), // child's own ID
        (1, 1, links(10, ROOT, ROOT, Some((10, ROOT)))),
        (4, 1, links(10, ROOT, ROOT, Some((10, ROOT)))),
        (3, 1, links(10, ROOT, ROOT, Some((10, ROOT)))),
        (1, 1, links(11, CHILD, ROOT, None)),
        (3, 0, valid),
    ] {
        let bad = with_schema(event(stmt, 17, seq, refs));
        assert!(reader.render(&message(bad.clone())).is_err(), "{bad}");
    }
    assert!(reader.statement(2).is_none());
    reader.render(&message(root)).unwrap();
}

#[test]
fn partial_streams_require_consistent_id_address_pairs_even_before_the_start_arrives() {
    let mut reader = Renderer::default();
    reader
        .render(&message(with_schema(event(
            1,
            20,
            0,
            links(11, CHILD, ROOT, None),
        ))))
        .unwrap();
    let child_addr = RecordAddress::parse_span_start_ref(CHILD).unwrap();
    assert_eq!(reader.span_start(child_addr), None);
    let gap = "log.thread:17/span.start:1";
    let mut other_trace = links(11, CHILD, ROOT, None);
    other_trace[0] = json!({"trace.start:00000000000000000000000000000001":ROOT});
    for bad in [
        event(1, 17, 0, json!([])), // trace root claimed but not yet seen
        event(1, 20, 1, links(10, CHILD, ROOT, None)), // two IDs at one address
        event(1, 20, 1, links(11, gap, ROOT, None)), // one ID at two addresses
        event(1, 20, 1, other_trace),
        event(1, 20, 1, links(11, CHILD, gap, None)), // trace ID with another root
        event(2, 17, 2, links(11, CHILD, ROOT, Some((12, GRANDCHILD)))), // known non-start parent
    ] {
        assert!(reader.render(&message(bad.clone())).is_err(), "{bad}");
    }
    reader
        .render(&message(event(2, 17, 0, links(10, ROOT, ROOT, None))))
        .unwrap();
    reader
        .render(&message(event(
            2,
            17,
            2,
            links(11, CHILD, ROOT, Some((10, ROOT))),
        )))
        .unwrap();
    assert_eq!(
        reader
            .span_start(child_addr)
            .unwrap()
            .parent
            .unwrap()
            .id
            .get(),
        10
    );
    // Same-thread gaps can be in another output; they aren't known non-starts.
    reader
        .render(&message(event(1, 17, 3, links(13, gap, ROOT, None))))
        .unwrap();
    assert_eq!(
        reader.span_start(RecordAddress::parse_span_start_ref(gap).unwrap()),
        None
    );
}

#[test]
fn inline_span_refs_are_nonrecursive_and_do_not_create_membership_or_attributes() {
    let mut reader = Renderer::default();
    let mut event1 = with_schema(event(1, 20, 0, json!([])));
    event1["args"] = json!([{"holder":[{"$ref":ROOT}]}]);
    let mut undeclared = event1.clone();
    undeclared["defs"]
        .as_object_mut()
        .unwrap()
        .remove("log.relation:span.start");
    assert!(reader.render(&message(undeclared)).is_err());
    let event1 = message(event1);
    assert_eq!(event1.span_links().unwrap(), None);
    assert_eq!(
        reader.render(&event1).unwrap(),
        format!("1970-01-01T00:00:00.000Z {{ holder: [{ROOT}] }}")
    );
    // Even an inline-only reference cannot later resolve to an ordinary event.
    assert!(reader.render(&message(event(1, 17, 0, json!([])))).is_err());
    reader
        .render(&message(event(2, 17, 0, links(10, ROOT, ROOT, None))))
        .unwrap();
    let mut top = event(1, 17, 1, json!([]));
    for bad in [
        "log.thread:17/span.start:00",
        "log.thread:17/span.start:1",
        CHILD,
        GRANDCHILD,
    ] {
        top["args"] = json!([{"$ref":bad}]);
        assert!(reader.render(&message(top.clone())).is_err(), "{bad}");
    }
    top["args"] = json!([{"$ref":ROOT}]);
    assert_eq!(
        reader.render(&message(top)).unwrap(),
        format!("1970-01-01T00:00:00.000Z {ROOT}")
    );
}

#[test]
fn failed_formatting_installs_neither_start_nor_forward_claim() {
    let mut reader = Renderer::default();
    let mut bad = with_schema(event(2, 17, 0, links(10, ROOT, ROOT, None)));
    bad["args"] = json!([[{"$ref":GRANDCHILD}, {"$ref":"#/args/1"}]]);
    assert!(reader.render(&message(bad)).is_err());
    assert!(reader.statement(2).is_none());
    assert!(
        reader
            .span_start(RecordAddress::parse_span_start_ref(ROOT).unwrap())
            .is_none()
    );
    reader
        .render(&message(with_schema(event(1, 20, 0, json!([])))))
        .unwrap();
    reader
        .render(&message(event(2, 17, 0, links(12, ROOT, ROOT, None))))
        .unwrap();
}

#[test]
fn late_parent_cannot_close_a_cycle() {
    let mut reader = Renderer::default();
    // Both children claim the same (as yet unseen) trace root. The first
    // child's parent arrives on another thread, so its parentage is not yet known.
    let b = "log.thread:20/span.start:1";
    let a = message(with_schema(event(
        2,
        17,
        2,
        links(11, CHILD, ROOT, Some((12, b))),
    )));
    reader.render(&a).unwrap();
    let cycle = message(event(2, 20, 1, links(12, b, ROOT, Some((11, CHILD)))));
    assert!(reader.render(&cycle).is_err());
    let valid = message(event(2, 20, 1, links(12, b, ROOT, Some((10, ROOT)))));
    reader.render(&valid).unwrap();
}
