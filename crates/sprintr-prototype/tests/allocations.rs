// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::buffer::{HeapBuffer, StackBuffer};
use sprintr_prototype::{Attribute, Logger, attribute, info};
use std::hint::black_box;
use std::io;

/// Scenario: a statement and its storage have been initialized before repeated recording.
/// Guarantees: escaped strings and full-width integers require no steady-state library allocations.
#[test]
fn warmed_heap_recording_does_not_allocate() {
    let text = "quoted \"text\"\n\\{braces}\u{03bb}".repeat(64);
    let value = u64::MAX;
    let mut logger = Logger::with_buffer(io::sink(), HeapBuffer::with_capacity(1));
    let mut emit = || info!("{text:s} {value:#04x}", logger: black_box(&mut logger)).unwrap();
    emit();
    let measured = allocation_counter::measure(|| {
        for _ in 0..128 {
            emit();
        }
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
    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<512>::new());
    let mut emit = || info!("{:s} {:04x}", logger: black_box(&mut logger), text, value).unwrap();
    emit();
    let measured = allocation_counter::measure(|| {
        for _ in 0..128 {
            emit();
        }
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Scenario: a statically attributed statement is reused with new argument values.
/// Guarantees: parameter binding and statement references allocate nothing on the warm producer path.
#[test]
fn repeated_attribute_recording_does_not_allocate() {
    static ACCOUNT: Attribute = attribute!("account.name", "Account name");
    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<1024>::new());
    let mut emit =
        |name| info!("hello {}", logger: black_box(&mut logger), ACCOUNT = name).unwrap();
    emit("Ada");
    let measured = allocation_counter::measure(|| {
        for name in ["Grace", "Ada"].into_iter().cycle().take(128) {
            emit(name);
        }
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}
