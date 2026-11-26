// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::buffer::{HeapBuffer, StackBuffer};
use sprintr_prototype::{Context, Logger, attributes, entities, info, span};
use std::io;

/// Scenario: a statement and its storage have been initialized before repeated recording.
/// Guarantees: escaped strings and full-width integers require no steady-state library allocations.
#[test]
fn warmed_heap_recording_does_not_allocate() {
    let text = "quoted \"text\"\n\\{braces}\u{03bb}".repeat(64);
    let value = u64::MAX;
    let mut logger = Logger::with_buffer(io::sink(), HeapBuffer::with_capacity(1));
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let emit = || info!("{text:s} {value:#04x}");
        emit();
        allocation_counter::measure(|| {
            for _ in 0..128 {
                emit();
            }
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Scenario: fixed stack storage and first-use statement bookkeeping are initialized.
/// Guarantees: repeated recording needs no heap allocation.
#[test]
fn warmed_stack_recording_does_not_allocate() {
    let text = "quoted \"text\"\n\\{braces}\u{03bb}";
    let value = u64::MAX;
    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<2048>::new());
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let emit = || info!("{:s} {:04x}", text, value);
        emit();
        allocation_counter::measure(|| {
            for _ in 0..128 {
                emit();
            }
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Scenario: a statically attributed statement is reused with new argument values.
/// Guarantees: parameter binding and statement references allocate nothing on the warm producer path.
#[test]
fn repeated_attribute_recording_does_not_allocate() {
    attributes! {
        /// Account name
        account: str = "account.name";
    }

    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<2048>::new());
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let emit = |name| info!("hello {}", account = name);
        emit("Ada");
        allocation_counter::measure(|| {
            for name in ["Grace", "Ada"].into_iter().cycle().take(128) {
                emit(name);
            }
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Reusing an entity should format only its address, not serialize the object again.
#[test]
fn repeated_entity_reference_does_not_allocate() {
    entities! {
        /// Application session
        #[anonymous]
        session = "session";
    }

    let login = session.new(serde_json::json!({"account": "Ada"})).unwrap();
    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<2048>::new());
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let emit = || info!("resumed {login}");
        emit();
        allocation_counter::measure(|| {
            for _ in 0..128 {
                emit();
            }
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
}

/// Scenario: the same nested formatter runs on a warmed caller-owned logger.
/// Guarantees: both argument encoding and outer association collection reuse storage.
#[test]
fn repeated_structured_recording_does_not_allocate() {
    let x = 3u32;
    let inner = sprintr_prototype::structured!("x={x}");
    let outer = sprintr_prototype::structured!("point({inner})");
    let mut logger = Logger::new(io::sink());
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let emit = || info!("{outer}");
        emit();
        allocation_counter::measure(|| {
            for _ in 0..128 {
                emit();
            }
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
}

/// Scenario: a worker repeatedly detaches and re-enters a prepared multi-role bag.
/// Guarantees: neither capturing the snapshot nor warm event encoding copies the bag.
#[test]
fn warmed_detached_context_does_not_allocate() {
    entities! {
        /// Local work item
        #[anonymous]
        work = "work";
    }
    attributes! {
        /// Current work item
        current: entity(work) = "work.current";
        /// Parent work item
        parent: entity(work) = "work.parent";
    }
    let handle = work.new(serde_json::json!({})).unwrap();
    let mut ctx = Context::root();
    ctx.insert(current.context(&handle).unwrap());
    ctx.insert(parent.context(&handle).unwrap());
    let mut logger = Logger::new(io::sink());
    logger.start().unwrap();
    fn emit() {
        info!("working");
    }
    let measured = logger.with_default(|| {
        ctx.in_scope(|| {
            emit();
            allocation_counter::measure(|| {
                for _ in 0..128 {
                    Context::current().in_scope(emit);
                }
            })
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
}

/// Scenario: warmed instrumentation logs under a span and refers to a holder.
/// Guarantees: neither implicit membership nor the explicit reference allocates.
#[test]
fn warmed_span_event_does_not_allocate() {
    let mut logger = Logger::new(io::sink());
    logger.start().unwrap();
    let measured = logger.with_default(|| {
        let holder_span = span!("holder");
        let holder = holder_span.reference().unwrap();
        let active = span!("active");
        active.in_scope(|| {
            let emit = || info!("blocked on {holder}");
            emit();
            allocation_counter::measure(|| {
                for _ in 0..128 {
                    emit();
                }
            })
        })
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
}
