// SPDX-License-Identifier: Apache-2.0

mod common;

use sprintr_prototype::{
    Argument, ExceptionRef, Logger, Renderer, StructuredError, attributes, exception, info, span,
};
use std::{cell::Cell, error::Error, fmt};

#[derive(Debug, thiserror::Error)]
enum LoadError {
    #[error(transparent)]
    Port(StructuredError),
    #[error(transparent)]
    Config(StructuredError),
}

attributes! {
    /// Configuration revision
    revision: u64 = "config.revision";
    /// Failure reported to the operator
    problem: entity(sprintr_prototype::schema::exception) = "operator.problem";
}

struct CountDebug<'a>(&'a Cell<u32>);
impl fmt::Debug for CountDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.set(self.0.get() + 1);
        f.write_str("metrics")
    }
}

fn leaf(calls: &Cell<u32>) -> StructuredError {
    let port = CountDebug(calls);
    let LoadError::Port(e) = exception!(
        LoadError::Port,
        "unknown {port:?} in rev {:#04x}",
        revision = 2u8
    ) else {
        unreachable!()
    };
    e
}

fn wrap(inner: &StructuredError) -> LoadError {
    let path = String::from("a\nb");
    exception!(LoadError::Config, "could not load {path:s}", cause = inner)
}

/// Scenario: a wrapped failure is mentioned twice and a second leaf has identical fields.
/// Guarantees: each capture has independent identity and one stack; mentions copy neither.
#[test]
fn captures_once_per_layer_and_mentions_only_reference_it() {
    let calls = Cell::new(0);
    let mut logger = common::started_logger();
    let (inner, outer, other) = logger.with_default(|| {
        let s = span!("load configuration");
        s.in_scope(|| {
            let inner = leaf(&calls);
            let outer = wrap(&inner);
            let LoadError::Config(e) = &outer else {
                unreachable!()
            };
            info!("reported {}", problem = e);
            info!("again {e}");
            (inner, outer, leaf(&calls))
        })
    });
    let inner_ref = inner.reference().unwrap();
    assert_ne!(other.reference().unwrap().id(), inner_ref.id());
    assert_eq!(outer.to_string(), "could not load a\nb");
    assert_eq!(
        outer.source().unwrap().to_string(),
        "unknown metrics in rev 0x02"
    );
    assert_eq!(calls.get(), 2);
    let LoadError::Config(outer) = outer else {
        unreachable!()
    };
    let outer_ref = outer.reference().unwrap();
    let lines = common::lines(&logger.into_inner());
    // startup, span start, leaf, wrapper, two mentions, another leaf, span end.
    assert_eq!(lines.len(), 8);
    assert_eq!(lines[2].stack, inner.stack());
    assert_eq!(lines[3].stack, outer.stack());
    assert_eq!(
        lines[2].span_links().unwrap(),
        lines[3].span_links().unwrap()
    );
    assert!(lines[2].span_links().unwrap().is_some());
    assert_eq!(
        common::template(&lines[2]).source.line,
        inner.location().line()
    );
    assert_ne!(inner.location().line(), outer.location().line());
    assert_eq!(
        lines[2].args,
        [Argument::String("metrics".into()), Argument::Unsigned(2)]
    );
    for event in &lines[4..6] {
        assert_eq!(
            event.args,
            [Argument::Reference {
                target: outer_ref.to_string()
            }]
        );
        assert!(!event.defs.keys().any(|k| k.starts_with("exception:")));
    }
    let mut reader = Renderer::default();
    for e in lines {
        reader.render(&e).unwrap();
    }
    let chain = reader
        .exception_chain(outer_ref.origin(), |_| None)
        .unwrap();
    assert!(chain.starts_with("LoadError::Config: could not load a\nb\n    at "));
    assert!(chain.contains("\nCaused by: LoadError::Port: unknown metrics in rev 0x02\n    at "));
    assert_eq!(calls.get(), 2);
}

/// Scenario: capture before startup and rejection by a started sink.
/// Guarantees: the application keeps the local chain and no false origin escapes.
#[test]
fn rejected_or_unstarted_capture_remains_a_local_source_without_an_origin() {
    let mut prestart = Logger::new(Vec::new());
    let unpublished = prestart.with_default(|| leaf(&Cell::new(0)));
    assert!(unpublished.reference().is_none());
    assert!(prestart.into_inner().is_empty());

    let mut logger = Logger::new(common::FailNth::new(2));
    logger.start().unwrap();
    let outer = logger.with_default(|| {
        let inner = leaf(&Cell::new(0));
        assert!(inner.reference().is_none());
        info!("previous error {inner}");
        wrap(&inner)
    });
    assert_eq!(outer.source().unwrap().to_string(), unpublished.to_string());
    let LoadError::Config(e) = outer else {
        unreachable!()
    };
    let mut reader = Renderer::default();
    let lines = common::lines(&logger.into_inner().output);
    assert_eq!(lines.len(), 3);
    reader.render(&lines[0]).unwrap();
    assert!(
        reader
            .render(&lines[1])
            .unwrap()
            .contains("<unrecorded exception:")
    );
    reader.render(&lines[2]).unwrap();
    let chain = reader
        .exception_chain(e.reference().unwrap().origin(), |_| None)
        .unwrap();
    assert!(chain.contains("Caused by: <unrecorded exception:"));
}

/// Scenario: playback sees the wrapping worker before the cause's worker.
/// Guarantees: a missing cause stays visible, and late resolution uses the cause's stack.
#[test]
fn transferable_cause_can_arrive_late_and_symbols_belong_to_each_origin() {
    fn transferable<T: Send + Sync>() {}
    transferable::<ExceptionRef>();
    let mut source = common::started_logger();
    let r = source.with_default(|| leaf(&Cell::new(0)).reference().unwrap());
    let (dest, outer) = std::thread::spawn(move || {
        let mut dest = common::started_logger();
        let outer = dest.with_default(|| {
            let LoadError::Config(e) = exception!(LoadError::Config, "worker failed", cause = &r)
            else {
                unreachable!()
            };
            assert_eq!(e.source().unwrap().to_string(), r.to_string());
            e.reference().unwrap()
        });
        (dest.into_inner(), outer)
    })
    .join()
    .unwrap();
    assert_ne!(r.origin().thread(), outer.origin().thread());
    let mut reader = Renderer::default();
    let mut destination = common::lines(&dest);
    destination[1].stack = vec![0x11, 0x33];
    for e in destination {
        reader.render(&e).unwrap();
    }
    assert!(
        reader
            .exception_chain(outer.origin(), |_| None)
            .unwrap()
            .ends_with(&format!("Caused by: <missing {r}>"))
    );
    let mut source = common::lines(&source.into_inner());
    source[1].stack = vec![0x22, 0x33];
    reader.render(&source[0]).unwrap();
    // A conflicting late definition must not claim this record sequence.
    let mut bad = source[1].clone();
    let key = format!("exception:{}", r.id());
    let def = bad.defs.remove(&key).unwrap();
    bad.defs
        .insert("exception:18446744073709551615".into(), def);
    assert!(reader.render(&bad).is_err());
    let mut cycle = source[1].clone();
    let sprintr_prototype::Definition::Entity(props) = cycle.defs.get_mut(&key).unwrap() else {
        unreachable!()
    };
    props.insert(
        "exception.cause".into(),
        serde_json::json!({"$ref": outer.to_string()}),
    );
    assert!(
        reader
            .render(&cycle)
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    reader.render(&source[1]).unwrap();
    let chain = reader
        .exception_chain(outer.origin(), |pc| {
            (pc == 0x22).then(|| "at original_leaf".into())
        })
        .unwrap();
    let (wrapper, leaf) = chain.split_once("\nCaused by: ").unwrap();
    assert!(wrapper.contains("0x11") && wrapper.contains("0x33"));
    assert!(leaf.ends_with("\n    at original_leaf\n    ... 1 more"));
}
