// SPDX-License-Identifier: Apache-2.0

mod common;

use serde_json::json;
use sprintr_prototype::{EntityHandle, IdentityValue, Renderer, attributes, entities, info};

attributes! {
    /// Pipeline incarnation
    run_uid: str = "run.uid";
    /// Name of the node within a pipeline incarnation
    node_id: str = "node.id";
    /// Output number on this node
    port_id: u64 = "port.id";
    /// Output signal
    signal: str = "port.signal";
    /// Node observed by this worker
    observed: entity(node) = "worker.observed";
}

entities! {
    /// Pipeline incarnation
    #[named(global, id = run_uid)]
    run = "run";
    /// Node in a particular pipeline incarnation
    #[named(within = run, id = node_id)]
    node = "node";
    /// Output of a particular node
    #[named(within = node, id = (port_id, signal))]
    port = "port";
    /// Output identified within a particular span
    #[named(within = sprintr_prototype::schema::span_start, id = port_id)]
    span_output = "span.output";
}

fn node_in(uid: &str) -> EntityHandle {
    let enclosing = run.named(uid, json!({})).unwrap();
    node.within(&enclosing)
        .named("batch", json!({"kind":"processor"}))
        .unwrap()
}

/// Scenario: a child of a span is first recorded after the span ended, on a
/// different output. Guarantees: both IDs travel with the handle, independently
/// of the start record's location or the worker's active context.
#[test]
fn span_container_supplies_trace_and_span_identity() {
    let mut origin = common::started_logger();
    let span_ref = origin.with_default(|| {
        let owner = sprintr_prototype::span!("work");
        owner.reference().unwrap()
    });
    let trace_id = format!("{:032x}", span_ref.trace_id());
    let span_id = format!("{:016x}", span_ref.span_id());
    let output = span_output.within(&span_ref).named(3u8, json!({})).unwrap();
    drop(span_ref);

    let span = output.identity().within().unwrap();
    assert_eq!(span.declaration().id_keys, ["span.id"]);
    assert_eq!(span.values(), [IdentityValue::from(span_id.as_str())]);
    let trace = span.within().unwrap();
    assert_eq!(trace.declaration().id_keys, ["trace.id"]);
    assert_eq!(trace.values(), [IdentityValue::from(trace_id.as_str())]);
    let mut logger = common::started_logger();
    logger.with_default(|| info!("{output}"));
    let mut reader = Renderer::default();
    for event in common::lines(&logger.into_inner()) {
        reader.render(&event).unwrap();
    }
    let key = format!(r#"span.output:[["{trace_id}"],["{span_id}"],[3]]"#);
    let parts = reader.identity_components(&key).unwrap();
    assert_eq!(parts[0].values, trace.values());
    assert_eq!(parts[1].values, span.values());
    assert_eq!(parts[2].values, output.identity().values());
}

/// Scenario: equal local names exist in two runs; only their ports are logged.
/// Guarantees: the handle and wire schema retain typed ancestor keys, even after
/// the parents are dropped and without logging their definitions.
#[test]
fn identity_is_a_chain_of_declared_keys_not_a_creation_site() {
    let a = node_in("run-a");
    let b = node_in("run-b");
    let p = port
        .within(&a)
        .named((7u64, "logs"), json!({"rate":2}))
        .unwrap();
    let q = port
        .within(&b)
        .named((7u64, "logs"), json!({"rate":3}))
        .unwrap();
    drop((a, b));
    let leaf = p.identity();
    assert_eq!(leaf.declaration().id_keys, ["port.id", "port.signal"]);
    assert_eq!(leaf.values(), [IdentityValue::Unsigned(7), "logs".into()]);
    let root = leaf.within().unwrap().within().unwrap();
    assert_eq!(root.entity_type(), "run");
    assert_eq!(root.declaration().id_keys, ["run.uid"]);
    assert_eq!(root.values(), [IdentityValue::from("run-a")]);
    let mut logger = common::started_logger();
    logger.with_default(|| info!("{p} {q}"));
    let bytes = logger.into_inner();
    let event = common::records(&bytes).pop().unwrap();
    let key = r#"port:[["run-a"],["batch"],[7,"logs"]]"#;
    assert!(event.defs.contains_key(key));
    assert!(
        event
            .defs
            .contains_key(r#"port:[["run-b"],["batch"],[7,"logs"]]"#)
    );
    assert!(
        !event
            .defs
            .keys()
            .any(|k| k.starts_with("node:") || k.starts_with("run:"))
    );
    let mut reader = Renderer::default();
    for event in common::lines(&bytes) {
        reader.render(&event).unwrap();
    }
    let parts = reader.identity_components(key).unwrap();
    assert_eq!(parts[0].values, root.values());
    assert_eq!(parts[2].values, leaf.values());
    assert_eq!(
        reader
            .relation("node")
            .unwrap()
            .scope_entity_type
            .as_deref(),
        Some("run")
    );
}

/// Scenario: two handles re-create the same named identity and a third disagrees
/// on its definition. Guarantees: equal definitions reuse an accepted record;
/// a conflict within or across records does not publish an invalid stream.
#[test]
fn repeat_creation_and_conflicts_use_the_full_identity() {
    let a = node_in("one");
    let again = node_in("one");
    let owner = run.named("one", json!({})).unwrap();
    let different = node
        .within(&owner)
        .named("batch", json!({"kind":"other"}))
        .unwrap();
    assert!(
        port.within(&owner)
            .named((7u64, "logs"), json!({}))
            .is_err()
    );
    assert!(port.within(&a).named("7", json!({})).is_err());
    assert!(port.within(&a).named(("7", "logs"), json!({})).is_err());
    let mut logger = common::started_logger();
    logger.with_default(|| {
        info!("conflict {a} {different}");
        for handle in [&a, &again, &different, &again] {
            observed
                .context(handle)
                .unwrap()
                .in_scope(|| info!("using {handle}"));
        }
    });
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 3);
    assert!(events[0].defs.contains_key(r#"node:[["one"],["batch"]]"#));
    for next in &events[1..] {
        assert_eq!(next.args, events[0].args);
        assert_eq!(next.context, events[0].context);
        assert!(next.defs.is_empty());
    }
    assert_eq!(
        common::replayed(&bytes)
            .matches(r#"using {"kind":"processor"}"#)
            .count(),
        3
    );
}
