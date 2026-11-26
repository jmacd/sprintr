// SPDX-License-Identifier: Apache-2.0

mod common;

use sprintr_prototype::{Argument, Context, Logger, Renderer, attributes, entities, info, span};

attributes! {
    /// Partition processed by this operation
    partition: u64 = "work.partition";
}

/// Scenario: a root is re-entered, a child ends explicitly, and its reference
/// is logged after both owners have gone. Guarantees: one end per start;
/// entering, exiting and retaining a reference do not change parentage.
#[test]
fn owner_lifetime_is_separate_from_activation_and_references() {
    let mut logger = common::started_logger();
    logger.with_default(|| {
        let root = span!("process {}", partition = 3u32);
        assert!(Context::current().span().is_none());
        root.in_scope(|| info!("first entry"));
        let child = root.in_scope(|| span!("read batch"));
        let holder = child.reference().unwrap();
        let snapshot = child.context().clone();
        child.in_scope(|| info!("reading"));
        child.end();
        root.in_scope(|| info!("second entry"));
        drop(root);
        assert_eq!(snapshot.span().unwrap().start(), holder.start());
        info!("completed {holder}");
    });

    let bytes = logger.into_inner();
    let events = common::records(&bytes);
    assert_eq!(events.len(), 8);
    let root = events[0].span_links().unwrap().unwrap();
    let child = events[2].span_links().unwrap().unwrap();
    assert_eq!(root.current.address, events[0].address().unwrap().unwrap());
    assert_eq!(root.root.address, root.current.address);
    assert!(root.parent.is_none());
    assert_eq!(child.parent, Some(root.current));
    assert_eq!(child.root, root.root);
    for (i, start) in [(1, root), (3, child), (4, child), (5, root), (6, root)] {
        let membership = events[i].span_links().unwrap().unwrap();
        assert_eq!(
            (membership.current, membership.root, membership.parent),
            (start.current, start.root, None)
        );
    }
    let mut reader = Renderer::default();
    let lines = common::lines(&bytes);
    let texts: Vec<_> = lines.iter().map(|e| reader.render(e).unwrap()).collect();
    assert!(texts[1].contains("process 3\nlog.kind = SPAN_START, work.partition = 3"));
    assert_eq!(reader.span_start(child.current.address), Some(child));
    assert_eq!(
        reader
            .statement(events[4].attrs["log.statement"])
            .unwrap()
            .attrs["log.kind"],
        "SPAN_END"
    );
    assert_eq!(
        events[6].attrs["log.statement"],
        events[4].attrs["log.statement"]
    );
    assert!(events[7].span_links().unwrap().is_none());
    assert_eq!(
        events[7].args,
        [Argument::Reference {
            target: child.current.address.span_start_ref().to_string()
        }]
    );
}

/// Scenario: a different worker receives the single close right and a reference.
/// Guarantees: the destination's baggage and output stay local; the same span
/// starts on the source thread and ends exactly once on the destination thread.
#[test]
fn transfer_owner_uses_destination_baggage() {
    entities! {
        /// Execution site
        #[named(global)]
        site = "site" {
            /// Source
            A,
            /// Destination
            B,
        }
    }
    attributes! {
        /// Where the worker is running
        worker: entity(site) = "worker.site";
    }
    let mut source = common::started_logger();
    let owner = source.with_default(|| {
        worker
            .context(&site::A)
            .unwrap()
            .in_scope(|| span!("handoff").into_owner())
    });
    let observer = owner.reference().unwrap();
    let destination = std::thread::spawn(move || {
        let mut out = common::started_logger();
        out.with_default(|| {
            worker.context(&site::B).unwrap().in_scope(|| {
                owner.in_scope(|| info!("processing"));
                owner.end();
                info!("finished {observer}");
            })
        });
        out.into_inner()
    })
    .join()
    .unwrap();

    let source = source.into_inner();
    let starts = common::records(&source);
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].context["worker.site"].entity, "site:A");
    let start = starts[0].span_links().unwrap().unwrap();
    let dest = common::records(&destination);
    assert_eq!(dest.len(), 3);
    assert!(
        dest.iter()
            .all(|e| e.context["worker.site"].entity == "site:B")
    );
    assert_ne!(dest[1].attrs["log.thread"], start.current.address.thread());
    assert_eq!(dest[0].span_links().unwrap(), Some(start));
    assert_eq!(dest[1].span_links().unwrap(), Some(start));
    assert_eq!(common::template(&dest[1]).attrs["log.kind"], "SPAN_END");
    assert!(dest[2].span_links().unwrap().is_none());

    // Independent outputs can arrive in the opposite order.
    let mut reader = Renderer::default();
    for e in common::lines(&destination) {
        reader.render(&e).unwrap();
    }
    assert!(reader.span_start(start.current.address).is_none());
    for e in common::lines(&source) {
        reader.render(&e).unwrap();
    }
    assert_eq!(reader.span_start(start.current.address), Some(start));
}

/// Scenario: the output rejects a child's start. Guarantees: execution still
/// runs, no parent address is handed out as the child's, and no end is emitted.
#[test]
fn rejected_start_has_no_close_right() {
    let mut logger = Logger::new(common::FailNth::new(3));
    logger.start().unwrap();
    logger.with_default(|| {
        let parent = span!("parent");
        parent.in_scope(|| {
            let rejected = span!("rejected");
            assert!(rejected.reference().is_none());
            rejected.in_scope(|| info!("still running"));
        });
    });
    let entries = logger.into_inner();
    assert_eq!(entries.writes, 5);
    let events = common::records(&entries.output);
    assert_eq!(events.len(), 3);
    assert!(events[1].span_links().unwrap().is_none());
    assert_eq!(
        events[2].span_links().unwrap(),
        events[0].span_links().unwrap()
    );
    assert!(common::replayed(&entries.output).contains("span ended\nlog.kind = SPAN_END"));
}
