// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{lines, records, replayed, started_logger};
use serde_json::json;
use sprintr_prototype::buffer::StackBuffer;
use sprintr_prototype::{
    Argument, Definition, EntityHandle, Logger, Message, RECORD, Renderer, THREAD, entities, info,
};
use std::io::Write;

entities! {
    /// Application session
    #[anonymous]
    session = "session";
}

fn emit<W: Write>(logger: &mut Logger<W>, handle: &EntityHandle) {
    logger.with_default(|| info!("opened {handle}"))
}

fn target(record: &Message) -> &str {
    let Argument::Reference { target } = &record.args[0] else {
        panic!("entity reference")
    };
    target
}

/// First use of a handle, not the callsite, determines where its object lives.
#[test]
fn defines_and_reuses_application_values() {
    let mut value = json!({"attrs": {}, "refs": [], "msg": "not a template"});
    let login = session.new(&value).unwrap();
    value["msg"] = "another session".into();
    let other = session.new(&value).unwrap();
    assert!(session.new(42).is_err());
    let mut logger = started_logger();
    emit(&mut logger, &login);
    emit(&mut logger, &other);
    logger.with_default(|| info!("resumed {} {}", login, login));
    let bytes = logger.into_inner();
    let events = records(&bytes);
    let (a, b, reuse) = (&events[0], &events[1], &events[2]);
    let (key, def) = a
        .defs
        .iter()
        .find(|(k, _)| k.starts_with("session:"))
        .unwrap();
    assert!(matches!(def, Definition::Entity(_)));
    assert_eq!(target(a), format!("{}#/defs/{key}", a.attrs[RECORD]));
    assert_eq!(a.attrs["log.statement"], b.attrs["log.statement"]);
    assert_eq!(b.defs.len(), 1);
    assert_ne!(target(a), target(b));
    assert!(!reuse.defs.contains_key(key));
    assert_eq!(reuse.args, [a.args[0].clone(), a.args[0].clone()]);
    assert!(replayed(&bytes).contains(r#"resumed {"attrs":{},"msg":"not a template","refs":[]}"#));

    // The key exists in record a, not record b. A reader must resolve the
    // address, not just look up the last definition with a matching key.
    let mut renderer = Renderer::default();
    for event in lines(&bytes).iter().take(3) {
        renderer.render(event).unwrap();
    }
    let mut bad = reuse.clone();
    bad.args[0] = Argument::Reference {
        target: format!("{}#/defs/{key}", b.attrs[RECORD]),
    };
    assert!(renderer.render(&bad).is_err());
    assert!(renderer.render(reuse).is_ok());
}

/// A logger can move across threads; each handle stays on its owning thread.
#[test]
fn local_handles_resolve_after_moving_a_logger_or_changing_sinks() {
    let login = session.new(json!({"account":"Ada"})).unwrap();
    let mut logger = started_logger();
    emit(&mut logger, &login);
    let worker = std::thread::scope(|s| {
        s.spawn(|| {
            let local = session.new(json!({"account":"Ada"})).unwrap();
            emit(&mut logger, &local);
            gettid::gettid()
        })
        .join()
        .unwrap()
    });
    emit(&mut logger, &login);
    let bytes = logger.into_inner();
    let startup = &lines(&bytes)[0];
    assert_eq!(startup.attrs[RECORD], 0);
    assert_eq!(startup.attrs[THREAD], gettid::gettid());
    let events = records(&bytes);
    let (a, b, c) = (&events[0], &events[1], &events[2]);
    let key = a.defs.keys().find(|k| k.starts_with("session:")).unwrap();
    assert_ne!(a.attrs[THREAD], b.attrs[THREAD]);
    assert_eq!((b.attrs[THREAD], b.attrs[RECORD]), (worker, 0));
    let worker_key = b.defs.keys().find(|k| k.starts_with("session:")).unwrap();
    assert_ne!(key, worker_key);
    assert!(!b.defs.contains_key(key));
    assert!(
        b.defs
            .contains_key(&format!("log.thread:{}", b.attrs[THREAD]))
    );
    assert_eq!(target(b), format!("0#/defs/{worker_key}"));
    assert_eq!(target(c), target(a));
    assert!(c.defs.is_empty());
    assert_eq!(
        replayed(&bytes)
            .matches(r#"opened {"account":"Ada"}"#)
            .count(),
        3
    );

    let mut another = started_logger();
    emit(&mut another, &login);
    let another = records(&another.into_inner()).remove(0);
    assert!(another.defs.contains_key(key));
    assert_eq!(another.attrs[THREAD], a.attrs[THREAD]);
    assert!(another.attrs[RECORD] > c.attrs[RECORD]);
    assert_eq!(
        target(&another),
        format!("{}#/defs/{key}", another.attrs[RECORD])
    );
}

#[test]
fn rejected_first_use_is_not_a_reference_target() {
    let login = session.new(json!({"account":"Ada"})).unwrap();
    let mut logger = Logger::with_buffer(Vec::new(), StackBuffer::<2048>::new());
    logger.start().unwrap();
    let mut log = |note: &str| logger.with_default(|| info!("{login} {note:s}"));
    log(&"x".repeat(4096));
    log("first accepted");
    log("again");
    let bytes = logger.into_inner();
    let events = records(&bytes);
    assert_eq!(events.len(), 2);
    assert!(target(&events[0]).starts_with(&format!("{}#", events[0].attrs[RECORD])));
    assert_eq!(target(&events[1]), target(&events[0]));
    assert!(events[1].defs.is_empty());
    assert!(replayed(&bytes).contains(r#"{"account":"Ada"} first accepted"#));
}
