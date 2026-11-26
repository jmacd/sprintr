// SPDX-License-Identifier: Apache-2.0

use super::*;
use IdentityValue::{String as Text, Unsigned};

fn schema() -> Value {
    let mut wire = snapshot();
    let defs = &mut wire["defs"];
    defs["log.relation:label"] = json!({"kind":"attribute","type":"string","desc":""});
    defs["log.relation:number"] = json!({"kind":"attribute","type":"u64","desc":""});
    defs["log.relation:owner"] = json!({"kind":"attribute","type":{"entity":"node"},"desc":""});
    defs["log.relation:run"] =
        json!({"kind":"entity","type":"object","desc":"","id_scope":"global","id_keys":["label"]});
    defs["log.relation:node"] = json!({"kind":"entity","type":"object","desc":"",
        "id_scope":"local","scope_entity_type":"run","id_keys":["number","label"]});
    wire
}

#[test]
fn nested_runtime_ids_are_typed_and_independent_of_parent_delivery() {
    let mut reader = Renderer::default();
    let mut wire = schema();
    wire["seq"] = 0.into();
    wire["thread"] = 17.into();
    // Neither enclosing instance has a definition in this stream.
    wire["defs"]["log.relation:run"]["id_scope"] = "local".into();
    wire["defs"]["log.relation:run"]["scope_entity_type"] = "stage".into();
    wire["defs"]["log.relation:stage"] =
        json!({"kind":"entity","type":"object","desc":"","id_scope":"global"});
    let key = r#"node:[["PROD"],["r:/~#%"],[18446744073709551615,"\"β\n"]]"#;
    let object = json!({"desc":false, "opaque":{"$ref":"unresolved"}});
    wire["defs"][key] = object.clone();
    wire["defs"]["log.statement:1"]["refs"] = json!([{"owner":{"$arg":0}}]);
    wire["args"] = json!([{"$ref":definition_fragment(key)}]);
    wire["attrs"]["owner"] = json!({"$ref":key});
    let first = message(wire.clone());
    assert_eq!(serde_json::to_value(&first).unwrap(), wire);
    assert!(reader.render(&first).unwrap().contains(&object.to_string()));
    assert_eq!(
        reader.relation("node").unwrap().id_keys,
        ["number", "label"]
    );
    assert_eq!(
        reader.identity_components(key).unwrap(),
        [
            IdentityComponent {
                entity_type: "stage".into(),
                values: vec![Text("PROD".into())]
            },
            IdentityComponent {
                entity_type: "run".into(),
                values: vec![Text("r:/~#%".into())]
            },
            IdentityComponent {
                entity_type: "node".into(),
                values: vec![Unsigned(u64::MAX), Text("\"β\n".into())]
            },
        ]
    );

    // An ordinary $ref resolves at its selected record; context looks up the
    // full entity key. Neither can fall back to matching just the leaf ID.
    let mut next = wire;
    next["seq"] = 1.into();
    next.as_object_mut().unwrap().remove("defs");
    next["args"][0]["$ref"] = format!("0{}", definition_fragment(key)).into();
    let other = key.replace("PROD", "TEST");
    let mut bad_arg = next.clone();
    bad_arg["args"][0]["$ref"] = format!("0{}", definition_fragment(&other)).into();
    assert!(reader.render(&message(bad_arg)).is_err());
    let mut bad_context = next.clone();
    bad_context["attrs"]["owner"]["$ref"] = other.clone().into();
    assert!(reader.render(&message(bad_context)).is_err());
    let mut conflict = next.clone();
    conflict["defs"] = json!({key: {"changed":true}});
    assert!(reader.render(&message(conflict)).is_err());
    reader.render(&message(next.clone())).unwrap();
    next["seq"] = 2.into();
    next["defs"] = json!({&other: {"changed":true}});
    next["attrs"]["owner"]["$ref"] = other.into();
    reader.render(&message(next)).unwrap();
}

#[test]
fn static_legacy_and_process_local_roots_are_not_equivalent() {
    let mut reader = Renderer::default();
    let mut wire = schema();
    wire["defs"]["log.relation:run"] = json!({"kind":"entity","type":"object","desc":""});
    let key = r#"node:[["PROD"],[0,"0"]]"#;
    wire["defs"][key] = json!({});
    assert!(reader.render(&message(wire.clone())).is_err());
    wire["defs"]["log.relation:run"]["id_scope"] = "global".into();
    wire["defs"]["run:PROD"] = json!({"not":"a static definition"});
    assert!(reader.render(&message(wire.clone())).is_err());
    wire["defs"]["run:PROD"] = json!({"desc":""});
    reader.render(&message(wire.clone())).unwrap();
    assert_eq!(
        reader.identity_components("run:PROD").unwrap()[0].values,
        [Text("PROD".into())]
    );

    let mut reader = Renderer::default();
    wire["defs"]["log.relation:node"]["scope_entity_type"] = "session".into();
    let defs = wire["defs"].as_object_mut().unwrap();
    defs.remove("session:9");
    defs.remove(key);
    let key = r#"node:[[1],[0,"0"]]"#;
    wire["defs"][key] = json!({});
    wire["args"] = json!([{"$ref":definition_fragment(key)}]);
    reader.render(&message(wire)).unwrap();
    let root = &reader.identity_components(key).unwrap()[0];
    assert_eq!(root.entity_type, "session");
    assert_eq!(root.values, [Unsigned(1)]);
    for bad in [r#"node:[[0],[0,"0"]]"#, r#"node:[["1"],[0,"0"]]"#] {
        assert!(reader.identity_components(bad).is_err(), "{bad}");
    }
}

#[test]
fn qualified_keys_require_exact_components_scalar_types_and_canonical_json() {
    let mut reader = Renderer::default();
    let mut wire = schema();
    for bad in [
        "plain",
        "null",
        "[]",
        r#"[["r"]]"#,
        r#"[["r"],[1,"n"],["extra"]]"#,
        r#"["r",[1,"n"]]"#,
        r#"[[],[1,"n"]]"#,
        r#"[[1],[1,"n"]]"#,
        r#"[["r"],[1]]"#,
        r#"[["r"],["n",1]]"#,
        r#"[["r"],["1","n"]]"#,
        r#"[["r"],[-1,"n"]]"#,
        r#"[["r"],[1.0,"n"]]"#,
        r#"[["r"],[true,"n"]]"#,
        r#"[["r"],[18446744073709551616,"n"]]"#,
        r#"[["r"],[0,{"$ref":"n"}]]"#,
        r#"[["r"], [1,"n"]]"#,
        r#"[["r"],[1,"n"]] "#,
        r#"[["\u0072"],[1,"n"]]"#,
        r#"[["r"],[1,"\/"]]"#,
    ] {
        let key = format!("node:{bad}");
        wire["defs"][&key] = json!({});
        assert!(reader.render(&message(wire.clone())).is_err(), "{key}");
        wire["defs"].as_object_mut().unwrap().remove(&key);
    }
    assert!(reader.relation("run").is_none());
    wire["defs"][r#"run:[["r"]]"#] = json!({"label":"not part of identity"});
    wire["defs"][r#"node:[["r"],[0,""]]"#] = json!({});
    reader.render(&message(wire)).unwrap();
}

#[test]
fn reject_invalid_identity_schemas_before_installing_any_declaration() {
    let mut reader = Renderer::default();
    let wire = schema();
    for (name, field, value) in [
        ("label", "id_scope", json!("global")),
        ("number", "id_keys", json!([])),
        ("owner", "scope_entity_type", json!("run")),
        ("session", "id_scope", json!("local")),
        ("session", "id_keys", json!(["number"])),
        ("run", "id_scope", json!("GLOBAL")),
        ("run", "id_scope", Value::Null),
        ("run", "scope_entity_type", json!("session")),
        ("node", "scope_entity_type", Value::Null),
        ("node", "scope_entity_type", json!("")),
        ("node", "scope_entity_type", json!("missing")),
        ("node", "scope_entity_type", json!("owner")),
        ("node", "scope_entity_type", json!("node")),
        ("node", "id_keys", json!(["label", "label"])),
        ("node", "id_keys", json!(["log.stack"])),
        ("node", "id_keys", json!(["owner"])),
        ("node", "id_keys", json!(["run"])),
        ("node", "id_keys", json!(["undefined"])),
        ("node", "id_keys", json!([7])),
        ("owner", "type", json!({"entity":"number"})),
    ] {
        let mut bad = wire.clone();
        bad["defs"][format!("log.relation:{name}")][field] = value;
        if let Ok(bad) = serde_json::from_value::<Message>(bad) {
            assert!(reader.render(&bad).is_err(), "{name} {field}");
        }
    }
    for (name, field) in [
        ("run", "id_scope"),
        ("node", "scope_entity_type"),
        ("node", "id_scope"),
    ] {
        let mut bad = wire.clone();
        bad["defs"][format!("log.relation:{name}")]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(reader.render(&message(bad)).is_err(), "{name} {field}");
    }
    let mut cycle = wire.clone();
    cycle["defs"]["log.relation:run"]["id_scope"] = "local".into();
    cycle["defs"]["log.relation:run"]["scope_entity_type"] = "node".into();
    assert!(reader.render(&message(cycle)).is_err());
    assert!(reader.statement(1).is_none());
    assert!(reader.relation("node").is_none());
    reader.render(&message(wire)).unwrap();
}
