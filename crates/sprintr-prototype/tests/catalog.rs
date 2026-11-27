// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::{
    Logger, RelationType, StructuredError, attributes, entities, exception, info,
    schema::{SiteKind, SlotCapture, catalog},
    structured,
};

attributes! {
    /// Identifier inside its pipeline
    local_id: str = "node.id";
    /// Deliberately different from the SDK's unsigned PID
    pid_text: str = "process.pid";
}
entities! {
    /// Linked test pipeline
    #[named(global)]
    pipeline = "pipeline";
    /// Node scoped to a pipeline
    #[named(within = pipeline, id = local_id)]
    node = "node";
}

fn not_evaluated() -> &'static str {
    panic!("catalog evaluated a callsite")
}

#[allow(dead_code)]
fn cold(inner: &StructuredError, n: u16) -> StructuredError {
    let nested = structured!("nested {} {n:#04x}", local_id = not_evaluated());
    info!("cold {} {n:?} {nested}", pid_text = not_evaluated());
    enum Failure {
        Wrapped(StructuredError),
    }
    let Failure::Wrapped(err) = exception!(Failure::Wrapped, "wrapping", cause = inner);
    err
}

/// Scenario: export before startup, including code which has never executed.
/// Guarantees: scopes, slot types, collision renames and causes are visible,
/// without user expression evaluation or spending the first runtime statement ID.
#[test]
fn cold_catalog_matches_the_linked_schema() {
    let c = catalog().unwrap();
    assert_eq!(
        serde_json::to_string(&c).unwrap(),
        serde_json::to_string(&catalog().unwrap()).unwrap()
    );
    let node_schema = &c.relations["node"].definition;
    assert_eq!(node_schema.id_keys, ["node.id"]);
    assert_eq!(node_schema.scope_entity_type.as_deref(), Some("pipeline"));
    let site = |fmt| {
        &c.sites
            .iter()
            .find(|s| s.descriptor.source_format == fmt)
            .unwrap()
            .descriptor
    };
    let cold = site("cold {} {n:?} {nested}");
    let attr = cold.arguments[0].attribute.as_ref().unwrap();
    assert!(attr.starts_with("process.pid_"));
    assert_eq!(c.relations[attr].origins[0].declared, "process.pid");
    assert_eq!(
        cold.recorded_format,
        format!("cold {{@{attr}}} {{n}} {{nested}}")
    );
    assert_eq!(cold.arguments[1].capture, SlotCapture::DebugText);
    assert_eq!(cold.arguments[1].wire_type, Some(RelationType::String));
    assert_eq!(cold.arguments[2].capture, SlotCapture::Dynamic);
    assert_eq!(cold.arguments[2].wire_type, None);
    let nested = site("nested {} {n:#04x}");
    assert_eq!(nested.kind, SiteKind::Structure);
    assert_eq!(nested.arguments[1].wire_type, Some(RelationType::U64));
    assert_eq!(nested.arguments[1].specifier, "#04x");
    let wrapped = site("wrapping");
    assert!(wrapped.has_cause && wrapped.arguments.is_empty());
    assert_eq!(wrapped.attrs["exception.class"], "Failure::Wrapped");
    let mut logger = Logger::new(Vec::new());
    logger.start().unwrap();
    let data = logger.into_inner();
    let first: serde_json::Value =
        serde_json::from_slice(data.split(|&b| b == b'\n').next().unwrap()).unwrap();
    assert_eq!(first["stmt"], 1);
}
