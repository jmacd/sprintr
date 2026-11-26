// SPDX-License-Identifier: Apache-2.0

mod common;

use serde_json::json;
use sprintr_prototype::{
    Context, ContextEntity, Logger, Renderer, attributes, context, entities, info,
};

entities! {
    /// Application session
    #[anonymous]
    session = "session";
    /// Deployment environment
    #[named(global)]
    environment = "environment" {
        /// Production
        PROD,
    }
}
attributes! {
    /// Current session
    active: entity(session) = "request.session";
    /// Parent session
    parent: entity(session) = "request.parent";
    /// Current environment
    env: entity(environment) = "deployment.env";
    /// Job number in a creation statement
    job_id: u64 = "job.id";
}

fn emit() {
    info!("ready");
}

/// Scenario: one creation callsite is used both in a role scope and in an
/// already-addressed context. Guarantees: each result inherits the bindings
/// but gets its own origin; activation only changes the current bag.
#[test]
fn context_origins_are_accepted_start_records() {
    fn create(id: u32) -> Context {
        context!("new job {:04x}", job_id = id)
    }

    let mut logger = common::started_logger();
    let role = env.context(&environment::PROD).unwrap();
    let (outer, child) = logger.with_default(|| role.in_scope(|| {
        let outer = create(42);
        assert!(Context::current().origin().is_none());
        let child = outer.in_scope(|| {
            let child = create(43);
            assert_eq!(Context::current().origin(), outer.origin());
            child
        });
        for ctx in [&outer, &child] {
            ctx.in_scope(|| {
                assert_eq!(Context::current().origin(), ctx.origin());
                assert!(matches!(Context::current().get(&env).unwrap(), Some(ContextEntity::Named(n)) if std::ptr::eq(n, &environment::PROD)));
            });
        }
        (outer, child)
    }));

    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 2);
    let mut reader = Renderer::default();
    let mut text = String::new();
    for event in common::lines(&bytes) {
        text = reader.render(&event).unwrap();
    }
    assert!(text.contains("new job 002b"));
    let statement = events[0].attrs["log.statement"];
    assert_eq!(events[1].attrs["log.statement"], statement);
    let template = reader.statement(statement).unwrap();
    assert_eq!(template.attrs["log.kind"], "CONTEXT_START");
    assert_eq!(template.refs[0]["job.id"].argument, 0);
    assert!(events[0].defs.contains_key("log.kind:CONTEXT_START"));
    assert!(events[1].defs.is_empty());
    for (ctx, event) in [(&outer, &events[0]), (&child, &events[1])] {
        let origin = ctx.origin().unwrap();
        assert_eq!(Some(origin), event.address().unwrap());
        assert_eq!(origin.thread(), event.attrs["log.thread"]);
        assert_eq!(origin.sequence(), event.attrs["log.record"]);
        assert_eq!(event.context["deployment.env"].entity, "environment:PROD");
    }
    assert_ne!(outer.origin(), child.origin());
}

/// Scenario: startup and a parent start succeed, then the sink rejects the
/// child's first record. Guarantees: no synthetic or inherited origin on
/// failure, and the next accepted start publishes its definitions normally.
#[test]
fn rejected_context_start_keeps_bindings_but_not_parent_origin() {
    fn child() -> Context {
        context!("child")
    }
    let mut logger = Logger::new(common::FailNth::new(3));
    logger.start().unwrap();
    let (outer, accepted) = logger.with_default(|| {
        let outer = env
            .context(&environment::PROD)
            .unwrap()
            .in_scope(|| context!("parent"));
        let accepted = outer.in_scope(|| {
            let rejected = child();
            assert!(rejected.origin().is_none());
            assert!(rejected.get(&env).unwrap().is_some());
            child()
        });
        (outer, accepted)
    });
    let sink = logger.into_inner();
    let events = common::records(&sink.output);
    assert_eq!(events.len(), 2);
    assert_eq!(
        outer.origin().unwrap(),
        events[0].address().unwrap().unwrap()
    );
    assert_eq!(
        accepted.origin().unwrap(),
        events[1].address().unwrap().unwrap()
    );
    assert_eq!(common::template(&events[1]).msg, "child");
    assert!(common::replayed(&sink.output).contains("child\nlog.kind = CONTEXT_START"));
}

/// Scenario: a work item outlives its creating scope and its original handle.
/// Guarantees: edits to the detached bag are isolated; full replacement and
/// unwind restore context without replacing the logger or resetting its sequence.
#[test]
fn stored_context_owns_an_independently_editable_snapshot() {
    struct Work {
        context: Context,
    }
    let mut logger = common::started_logger();
    let work = logger.with_default(|| {
        let a = session.new(json!({"name":"A"})).unwrap();
        active.context(&a).unwrap().in_scope(|| {
            env.context(&environment::PROD).unwrap().in_scope(|| {
                emit();
                let mut detached = Context::current();
                assert!(matches!(detached.get(&env).unwrap(), Some(ContextEntity::Named(n)) if std::ptr::eq(n, &environment::PROD)));
                let b = session.new(json!({"name":"B"})).unwrap();
                detached.insert(active.context(&b).unwrap());
                assert!(detached.remove(&env).unwrap());
                assert!(!detached.remove(&parent).unwrap());
                emit(); // still A and PROD
                Work { context: detached }
            })
        })
    });
    logger.with_default(|| {
        emit(); // no scope
        work.context.in_scope(|| {
            assert!(matches!(
                Context::current().get(&active).unwrap(),
                Some(ContextEntity::Handle(_))
            ));
            emit(); // B, after b and its creator's scope have gone away
            let unwound = std::panic::catch_unwind(|| {
                Context::root().in_scope(|| {
                    emit();
                    panic!("leave the empty root");
                })
            });
            assert!(unwound.is_err());
            emit(); // B restored
        });
        emit(); // no scope
    });
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 7);
    let a = &events[0].context["request.session"].entity;
    let b = &events[3].context["request.session"].entity;
    assert_ne!(a, b);
    assert_eq!(events[0].context.len(), 2);
    assert_eq!(events[0].context, events[1].context);
    assert_eq!(events[3].context.len(), 1);
    assert_eq!(events[3].context, events[5].context);
    assert!(events[3].defs.contains_key(b));
    for i in [2, 4, 6] {
        assert!(events[i].context.is_empty());
    }
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].attrs["log.record"] + 1 == pair[1].attrs["log.record"])
    );
    assert_eq!(common::replayed(&bytes).matches("ready\n").count(), 7);
}

/// Scenario: one callsite runs in nested scopes and across logger and thread boundaries.
/// Guarantees: context lives on the event, composes by role, and is restored on unwind.
#[test]
fn scoped_associations_follow_the_thread_not_the_logger_or_statement() {
    let a = session.new(json!({"name":"A"})).unwrap();
    let b = session.new(json!({"name":"B"})).unwrap();
    let outer = active.context(&a).unwrap();
    let inner = active.context(&b).unwrap();
    let ancestor = parent.context(&a).unwrap();
    let production = env.context(&environment::PROD).unwrap();
    assert!(active.context(&environment::PROD).is_err());
    assert!(env.context(&a).is_err());

    let mut logger = common::started_logger();
    let mut other = common::started_logger();
    let child = logger.with_default(|| {
        emit();
        let child = outer.in_scope(|| {
            production.in_scope(emit);
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                inner.in_scope(|| {
                    ancestor.in_scope(|| {
                        emit();
                        panic!("leave the scopes");
                    })
                });
            }));
            assert!(unwind.is_err());
            emit();
            other.with_default(emit);
            std::thread::spawn(|| {
                let mut logger = common::started_logger();
                logger.with_default(emit);
                logger.into_inner()
            })
            .join()
            .unwrap()
        });
        emit();
        child
    });
    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 5);
    assert!(events[0].context.is_empty() && events[4].context.is_empty());
    let a_key = &events[1].context["request.session"].entity;
    let b_key = &events[2].context["request.session"].entity;
    assert_ne!(a_key, b_key);
    assert!(events[1].defs.contains_key(a_key) && events[2].defs.contains_key(b_key));
    assert_eq!(&events[2].context["request.parent"].entity, a_key);
    assert!(!events[2].context.contains_key("deployment.env"));
    assert_eq!(events[3].context.len(), 1);
    assert_eq!(&events[3].context["request.session"].entity, a_key);
    assert!(events[3].defs.is_empty());
    assert!(events.iter().all(
        |e| e.attrs["log.statement"] == events[0].attrs["log.statement"]
            && e.args.is_empty()
            && e.refs.is_empty()
    ));
    assert_eq!(
        serde_json::to_value(&common::template(&events[0]).attrs).unwrap(),
        json!({"log.level":"INFO"})
    );
    let wire = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(wire["attrs"]["request.session"], json!({"$ref":a_key}));
    assert_eq!(
        wire["attrs"]["deployment.env"],
        json!({"$ref":"environment:PROD"})
    );
    assert_eq!(
        wire["defs"]["environment:PROD"],
        json!({"desc":"Production"})
    );
    assert!(common::replayed(&bytes).contains(&format!(
        "log.level = INFO, deployment.env = environment:PROD, request.session = {a_key}"
    )));
    assert!(
        common::records(&other.into_inner())[0]
            .defs
            .contains_key(a_key)
    );
    assert!(common::records(&child)[0].context.is_empty());
}

/// Scenario: a context-only first use is rejected by the output.
/// Guarantees: the next event introduces all required definitions and replay can resolve them.
#[test]
fn failed_first_use_does_not_consume_context_definitions() {
    let login = session.new(json!({"account":"Ada"})).unwrap();
    let binding = active.context(&login).unwrap();
    let mut logger = Logger::new(common::FailNth::new(2));
    logger.start().unwrap();
    binding.in_scope(|| {
        logger.with_default(|| {
            emit();
            emit();
            emit();
        })
    });
    let bytes = logger.into_inner().output;
    let events = common::records(&bytes);
    assert_eq!(events.len(), 2);
    let key = &events[0].context["request.session"].entity;
    assert!(events[0].defs.contains_key(key));
    assert!(events[0].defs.contains_key("log.relation:request.session"));
    assert!(events[1].defs.is_empty());
    assert_eq!(
        common::replayed(&bytes)
            .matches(&format!("request.session = {key}"))
            .count(),
        2
    );
}
