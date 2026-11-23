// SPDX-License-Identifier: Apache-2.0

mod common;

use serde_json::json;
use sprintr_prototype::{
    Logger, StructuredValue, Value, ValueWriter, attributes, entities, info, structured,
};
use std::io;

attributes! {
    /// Horizontal coordinate
    coordinate: u64 = "point.x";
    /// Session handling the request
    active_session: entity(session) = "request.session";
}

entities! {
    /// A session owned by an application object
    #[anonymous]
    session = "session";
    /// A different application entity
    #[anonymous]
    account = "account";
}

/// Scenario: one handle is the entire value of a wrapper and is also an
/// attributed positional argument. Guarantees: the inline reference supplies
/// identity; the formatted array and attribute supply only layout and role.
#[test]
fn entity_is_a_value_or_an_attributed_child() {
    struct Session(sprintr_prototype::EntityHandle);
    impl StructuredValue for Session {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            self.0.encode(out)
        }
    }
    struct Request<'a> {
        entity_handle: &'a sprintr_prototype::EntityHandle,
    }
    impl StructuredValue for Request<'_> {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            structured!("handling {}", active_session = self.entity_handle).encode(out)
        }
    }
    let value = Session(session.new(json!({"name":"Ada"})).unwrap());
    let request = Request {
        entity_handle: &value.0,
    };
    let wrong = account.new(json!({"name":"Wrong relation"})).unwrap();
    let mut logger = common::started_logger();
    logger.with_default(|| {
        info!("{value}");
        for r in [
            &Request {
                entity_handle: &wrong,
            },
            &request,
            &request,
        ] {
            info!("{r}");
        }
        info!("direct {}", active_session = value.0);
    });
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 4);
    let key = events[0]
        .defs
        .keys()
        .find(|k| k.starts_with("session:"))
        .unwrap();
    let slot = json!({"$ref": format!("{}#/defs/{key}", events[0].attrs["log.record"])});
    assert_eq!(
        serde_json::to_value(&events[0].args).unwrap(),
        json!([slot])
    );
    assert!(events[0].refs.is_empty());
    let encoder = events[1].refs[0].keys().next().unwrap();
    assert!(encoder.starts_with("struct.encode:Request_"));
    let expected = json!([{(encoder):"#/args/0"}, {"request.session":"#/args/0/0"}]);
    for r in &events[1..3] {
        assert_eq!(serde_json::to_value(&r.args).unwrap(), json!([[slot]]));
        assert_eq!(serde_json::to_value(&r.refs).unwrap(), expected);
        assert!(!r.defs.contains_key(key));
    }
    assert_eq!(
        serde_json::to_value(&events[1].defs["log.relation:request.session"]).unwrap()["type"],
        json!({"entity":"session"})
    );
    assert!(events[2].defs.is_empty());
    assert_eq!(
        serde_json::to_value(&events[3].args).unwrap(),
        json!([slot])
    );
    assert!(events[3].refs.is_empty());
    assert_eq!(
        serde_json::to_value(&common::template(&events[3]).refs).unwrap(),
        json!([{"request.session":{"$arg":0}}])
    );
    let text = common::replayed(&bytes);
    assert_eq!(text.matches("handling {\"name\":\"Ada\"}").count(), 2);
    assert_eq!(
        text.matches("request.session = {\"name\":\"Ada\"}").count(),
        3
    );
}

struct NamedPoint(u64, u64);
impl StructuredValue for NamedPoint {
    fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
        out.object(
            "Point",
            &[
                ("x", Value::Unsigned(self.0)),
                ("y", Value::Unsigned(self.1)),
            ],
        )
    }
}

#[test]
fn point_supplies_its_own_format() {
    struct Point(u64, u64);
    impl StructuredValue for Point {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            let Point(x, y) = *self;
            structured!("({x}, {y})").encode(out)
        }
    }

    let mut logger = common::started_logger();
    let point = Point(3, 4);
    let () = logger.with_default(|| info!("point = {point}"));
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    let wire = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(wire["args"], json!([[3, 4]]));
    let key = events[0].refs[0].keys().next().unwrap();
    let hash = key.strip_prefix("struct.encode:Point_").unwrap();
    assert!(!hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(wire["refs"], json!([{(key): "#/args/0"}]));
    let definition = &wire["defs"][key];
    assert_eq!(definition["msg"], "({x}, {y})");
    assert_eq!(definition["name"], std::any::type_name::<Point>());
    assert_eq!(definition["file"], file!());
    let line = definition["line"].as_u64().unwrap() as usize;
    assert!(
        include_str!("structured.rs")
            .lines()
            .nth(line - 1)
            .unwrap()
            .trim_start()
            .starts_with("structured!(")
    );
    assert_eq!(
        common::replayed(&bytes),
        "point = (3, 4)\nlog.level = INFO\n"
    );
}

/// Scenario: one callsite receives different dynamic types, including a named
/// object inside a formatted array, an attributed integer and a Debug trait object.
/// Guarantees: args retain literal data and all formatting lives in outer refs.
#[test]
fn nested_formats_and_objects() {
    fn emit(logger: &mut Logger<Vec<u8>>, value: &dyn StructuredValue) {
        logger.with_default(|| info!("value {value}"));
    }
    let mut logger = common::started_logger();
    let point = NamedPoint(3, 4);
    emit(&mut logger, &point);
    let debug: &dyn std::fmt::Debug = &vec![7u8, 8];
    let formatted = structured!("{} {:04x} {:?}", point, coordinate = 42u32, debug);
    emit(&mut logger, &formatted);
    emit(&mut logger, &formatted);
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    let wire = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(wire["args"], json!([[{"x":3,"y":4},42,"[7, 8]"]]));
    let key = events[1].refs[0].keys().next().unwrap();
    assert!(key.starts_with("struct.encode:"));
    assert_eq!(wire["defs"][key]["msg"], "{point} {@point.x:04x} {debug}");
    assert_eq!(
        wire["refs"],
        json!([
            {(key):"#/args/0"},
            {"struct.name:Point":"#/args/0/0"},
            {"point.x":"#/args/0/1"}
        ])
    );
    assert!(events[1].defs.contains_key("log.relation:struct.encode"));
    assert_eq!(events[1].refs, events[2].refs);
    assert!(events[2].defs.is_empty());
    assert!(common::replayed(&bytes).contains("value Point { x: 3, y: 4 } 002a [7, 8]"));
}

/// Scenario: a generic impl has one formatting callsite but multiple concrete
/// types; a logger moves between threads and its first definition write fails.
/// Guarantees: each variant has an unqualified key and each thread publishes
/// its own accepted definition before relying on it.
#[test]
fn encoder_definitions_follow_variant_output_and_thread() {
    struct Pair<T>(T);
    impl<T: Copy + Into<u64>> StructuredValue for Pair<T> {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            let n = self.0.into();
            structured!("({n})").encode(out)
        }
    }
    fn emit<W: io::Write>(logger: &mut Logger<W>, value: &dyn StructuredValue) {
        logger.with_default(|| info!("{value}"));
    }
    let a = Pair(5u8);
    let b = Pair(5u32);
    let mut logger = Logger::new(common::FailNth::new(2));
    logger.start().unwrap();
    emit(&mut logger, &a); // rejected at the sink
    emit(&mut logger, &a);
    emit(&mut logger, &b);
    std::thread::scope(|s| {
        s.spawn(|| {
            emit(&mut logger, &a);
            emit(&mut logger, &a);
        })
        .join()
        .unwrap()
    });
    emit(&mut logger, &a);
    let bytes = logger.into_inner().output;
    let events = common::records(&bytes);
    assert_eq!(events.len(), 5);
    let key_a = events[0].refs[0].keys().next().unwrap();
    let key_b = events[1].refs[0].keys().next().unwrap();
    assert_ne!(key_a, key_b);
    for (event, key, ty) in [
        (&events[0], key_a, std::any::type_name::<Pair<u8>>()),
        (&events[1], key_b, std::any::type_name::<Pair<u32>>()),
    ] {
        assert!(
            key.strip_prefix("struct.encode:Pair_")
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert_eq!(serde_json::to_value(&event.defs[key]).unwrap()["name"], ty);
    }
    assert_eq!(events[2].defs[key_a], events[0].defs[key_a]);
    assert!(
        events[3..]
            .iter()
            .all(|e| !e.defs.contains_key(key_a) && e.refs == events[0].refs)
    );
    assert_eq!(common::replayed(&bytes).matches("(5)").count(), 5);
    let mut other = common::started_logger();
    emit(&mut other, &a);
    assert_eq!(
        common::records(&other.into_inner())[0].defs[key_a],
        events[0].defs[key_a]
    );
}

/// Scenario: a structured field addresses an entity at a key containing
/// pointer escapes. The first attempted record exceeds its storage.
/// Guarantees: both nested definitions and locations follow record acceptance.
#[test]
fn nested_entity_definition_is_transactional() {
    struct Session<'a>(&'a sprintr_prototype::EntityHandle);
    impl StructuredValue for Session<'_> {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            out.object("Session", &[("a/~%", Value::Entity(self.0))])
        }
    }
    let handle = session.new(json!({"name":"Ada"})).unwrap();
    let value = Session(&handle);
    let mut logger = Logger::with_buffer(
        Vec::new(),
        sprintr_prototype::buffer::StackBuffer::<2048>::new(),
    );
    logger.start().unwrap();
    let mut emit = |text: &str| logger.with_default(|| info!("{value}{text:s}"));
    emit(&"x".repeat(4096));
    emit("first");
    emit("again");
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].args[1],
        sprintr_prototype::Argument::String("first".into())
    );
    let key = events[0]
        .defs
        .keys()
        .find(|k| k.starts_with("session:"))
        .unwrap();
    for event in &events {
        let wire = serde_json::to_value(event).unwrap();
        assert_eq!(wire["refs"], json!([{"struct.name:Session":"#/args/0"}]));
        assert_eq!(
            wire["args"][0]["a/~%"]["$ref"],
            format!("{}#/defs/{key}", events[0].attrs["log.record"])
        );
    }
    assert!(events[1].defs.is_empty());
    assert!(common::replayed(&bytes).contains("again"));
}

/// Scenario: a formatter visits itself, or revisits a child on a different branch.
/// Guarantees: a back-edge becomes a finite pointer; siblings remain independent.
#[test]
fn cycles_only_cut_active_ancestors() {
    struct Node(u64);
    impl StructuredValue for Node {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            out.object(
                "Node",
                &[
                    ("n", Value::Unsigned(self.0)),
                    ("next", Value::Structured(self)),
                ],
            )
        }
    }
    let node = Node(1);
    let pair = structured!("{node} {node}");
    let mut logger = common::started_logger();
    logger.with_default(|| info!("{pair}"));
    let bytes = logger.into_inner();
    let wire = serde_json::to_value(&common::records(&bytes)[0]).unwrap();
    assert_eq!(
        wire["args"],
        json!([[{"n":1,"next":{"$ref":"#/args/0/0"}},{"n":1,"next":{"$ref":"#/args/0/1"}}]])
    );
    assert!(common::replayed(&bytes).contains("#/args/0/0"));
}
