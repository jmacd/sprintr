// SPDX-License-Identifier: Apache-2.0

use slogr_prototype::buffer::{HeapBuffer, StackBuffer};
use slogr_prototype::{Logger, info, register_attribute};
use std::hint::black_box;
use std::io;

/// Scenario: a caller-owned logger has already grown to fit escaped string and integer arguments.
/// Guarantees: repeated direct JSON recording allocates no heap memory in the library.
#[test]
fn warmed_heap_recording_does_not_allocate() {
    let text = "quoted \"text\"\n\\{braces}\u{03bb}".repeat(64);
    let value = u64::MAX;
    let mut logger = Logger::with_buffer(io::sink(), HeapBuffer::with_capacity(1));
    info!("{text:s} {value:#04x}", logger: logger).unwrap();
    let measured = allocation_counter::measure(|| {
        for _ in 0..128 {
            info!(
                "{text:s} {value:#04x}",
                logger: black_box(&mut logger),
            )
            .unwrap();
        }
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Scenario: recording uses fixed stack storage, including the first record.
/// Guarantees: constructing the logger and encoding records requires no heap allocation.
#[test]
fn stack_recording_does_not_allocate() {
    let text = "quoted \"text\"\n\\{braces}\u{03bb}";
    let value = u64::MAX;
    let measured = allocation_counter::measure(|| {
        let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<512>::new());
        for _ in 0..128 {
            info!(
                "{:s} {:04x}",
                logger: black_box(&mut logger),
                text,
                value,
            )
            .unwrap();
        }
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}

/// Scenario: an attribute definition has been emitted and is reused with new argument values.
/// Guarantees: registration locks, definition encoding, and heap allocation stay off the steady-state path.
#[test]
fn repeated_attribute_recording_does_not_allocate() {
    let acct_name = register_attribute("account.name", "Account name").unwrap();
    let name = "Ada";
    let mut logger = Logger::with_buffer(io::sink(), StackBuffer::<512>::new());
    info!("hello {}", logger: logger, acct_name = name).unwrap();
    let measured = allocation_counter::measure(|| {
        for _ in 0..128 {
            info!("hello {}", logger: black_box(&mut logger), acct_name = name).unwrap();
        }
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(measured.bytes_total, 0, "{measured:?}");
}
