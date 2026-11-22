// SPDX-License-Identifier: Apache-2.0

mod common;

use common::{lines, records, replayed, template};
use sprintr_prototype::buffer::{HeapBuffer, RecordBuffer, StackBuffer};
use sprintr_prototype::{Argument, Logger, info, replay};
use std::cell::Cell;
use std::io::{self, Write};

/// One statement reached directly and via a caller shares its template, not its stack.
#[test]
fn repeated_statement_captures_the_current_call_path() {
    #[inline(never)]
    fn emit(logger: &mut Logger<Vec<u8>>) {
        info!("same statement", logger: logger).unwrap();
    }
    #[inline(never)]
    fn via_caller(logger: &mut Logger<Vec<u8>>) {
        emit(logger);
        std::hint::black_box(logger);
    }

    let mut logger = Logger::new(Vec::new());
    emit(&mut logger);
    via_caller(&mut logger);
    let events = records(&logger.into_inner());
    let (a, b) = (&events[0], &events[1]);
    assert_eq!(a.attrs["log.statement"], b.attrs["log.statement"]);
    assert!(b.defs.is_empty());
    for event in [a, b] {
        assert!((1..=10).contains(&event.stack.len()));
        assert!(!event.stack.contains(&0));
    }
    // Both events executed the same info! instruction. The first dynamic PC
    // differs because it belongs to its caller, not to that instruction.
    assert_ne!(a.stack[0], b.stack[0]);
}

/// Scenario: explicit positional expressions and implicit captures describe the same message.
/// Guarantees: the documented JSON retains native values and playback needs no source or tracing.
#[test]
fn positional_and_captured_roundtrip() {
    let name = String::from("Ada");
    let value = 42u32;
    let mut logger = Logger::new(Vec::new());
    info!("hello {:s}, value {:04x}", logger: logger, name, value).unwrap();
    info!("hello {name:s}, value {value:04x}", logger: logger).unwrap();
    logger.flush().unwrap();
    let encoded = logger.into_inner();
    let records = records(&encoded);
    let (a, b) = (template(&records[0]), template(&records[1]));
    assert_eq!((&a.msg, &a.refs, &a.attrs), (&b.msg, &b.refs, &b.attrs));
    assert_ne!(a.source.line, b.source.line);
    assert_eq!(template(&records[0]).msg, "hello {name}, value {value:04x}");
    assert_eq!(
        records[0].args,
        [Argument::String("Ada".into()), Argument::Unsigned(42)]
    );
    assert_eq!(records[0].args, records[1].args);
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO\nhello Ada, value 002a\nlog.level = INFO\n"
    );
    assert_eq!(name, "Ada");
}

/// Scenario: mixed implicit and explicit arguments include an AsRef-only value and a side effect.
/// Guarantees: recording borrows strings and evaluates each expression once at the logging call.
#[test]
fn capture_is_typed_borrowed_and_evaluated_once() {
    struct Text(String);
    impl AsRef<str> for Text {
        fn as_ref(&self) -> &str {
            &self.0
        }
    }
    let mut name = Text(String::from("Ada"));
    let calls = Cell::new(0);
    let next = || {
        calls.set(calls.get() + 1);
        42u64
    };
    let mut logger = Logger::new(Vec::new());
    info!("hello {name:s}, value {:04x}", logger: logger, next()).unwrap();
    name.0.clear();
    let encoded = logger.into_inner();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        records(&encoded)[0].args,
        [Argument::String("Ada".into()), Argument::Unsigned(42)]
    );
    assert_eq!(
        replayed(&encoded),
        "hello Ada, value 002a\nlog.level = INFO\n"
    );
}

/// Scenario: raw literals, escaped braces, repeated captures, and JSON-sensitive string data are logged.
/// Guarantees: data stays on one JSON line and is not interpreted as another formatting template.
#[test]
fn escaped_literals_and_string_data() {
    let text = "quoted \"text\"\n\\{braces}\t\u{03bb}";
    let mut logger = Logger::new(Vec::new());
    info!(r"{{{text:s}}} {text:s}", logger: logger).unwrap();
    info!("{{literal}}", logger: logger).unwrap();
    info!("", logger: logger).unwrap();
    let encoded = logger.into_inner();
    assert_eq!(encoded.iter().filter(|byte| **byte == b'\n').count(), 4);
    assert_eq!(
        replayed(&encoded),
        format!(
            "{{{text}}} {text}\nlog.level = INFO\n{{literal}}\nlog.level = INFO\n\nlog.level = INFO\n"
        )
    );
}

/// Scenario: direct encoding includes escaped format text, Unicode, braces, and a temporary argument.
/// Guarantees: decoded events serialize back to the same wire fields and temporaries are evaluated once.
#[test]
fn direct_encoding_roundtrips_through_the_reader() {
    let text = "quoted \"text\"\n\\{braces}\t\u{03bb}";
    let value = u64::MAX;
    let mut logger = Logger::new(Vec::new());
    info!("line\n{{{text:s}}} {value:#04x}", logger: logger).unwrap();
    let encoded = logger.into_inner();
    for (record, line) in lines(&encoded).iter().zip(encoded.split(|&b| b == b'\n')) {
        assert_eq!(
            serde_json::to_value(record).unwrap(),
            serde_json::from_slice::<serde_json::Value>(line).unwrap()
        );
    }
    assert_eq!(
        replayed(&encoded),
        format!("line\n{{{text}}} {value:#04x}\nlog.level = INFO\n")
    );

    let calls = Cell::new(0);
    let temporary = || {
        calls.set(calls.get() + 1);
        String::from("temporary")
    };
    let mut logger = Logger::new(Vec::new());
    info!("hello {:s}", logger: logger, temporary()).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        replayed(&logger.into_inner()),
        "hello temporary\nlog.level = INFO\n"
    );
}

/// Scenario: escaped argument data exceeds a bounded buffer between two successful writes.
/// Guarantees: neither storage strategy emits partial JSON or loses subsequent records.
#[test]
fn oversized_records_roll_back_and_allow_reuse() {
    fn check(buffer: impl RecordBuffer) {
        let mut logger = Logger::with_buffer(Vec::new(), buffer);
        info!("before", logger: logger).unwrap();
        let text = "\n".repeat(1100);
        assert_eq!(
            info!("{text:s}", logger: logger).unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        info!("after", logger: logger).unwrap();
        assert_eq!(
            replayed(&logger.into_inner()),
            "before\nlog.level = INFO\nafter\nlog.level = INFO\n"
        );
    }
    check(HeapBuffer::with_capacity_and_limit(16, 2048));
    check(StackBuffer::<2048>::new());
}

/// Scenario: the startup record exactly fills storage, or lacks room for its final newline.
/// Guarantees: limits apply to the complete wire record, including framing; nothing partial is written.
#[test]
fn buffer_limit_includes_record_framing() {
    fn emit(logger: &mut Logger<Vec<u8>>) -> io::Result<()> {
        info!("hello", logger: logger)
    }
    let mut sample = Logger::new(Vec::new());
    emit(&mut sample).unwrap();
    let output = sample.into_inner();
    let startup = output.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    assert_eq!(lines(&output).len(), 2);
    assert!(
        output.len() - startup < startup,
        "startup is the largest record"
    );
    let mut exact = Logger::with_buffer(
        Vec::new(),
        HeapBuffer::with_capacity_and_limit(startup, startup),
    );
    emit(&mut exact).unwrap();
    let exact = exact.into_inner();
    assert_eq!(
        exact.iter().position(|byte| *byte == b'\n').unwrap() + 1,
        startup
    );
    assert_eq!(replayed(&exact), replayed(&output));

    let mut short = Logger::with_buffer(
        Vec::new(),
        HeapBuffer::with_capacity_and_limit(startup, startup - 1),
    );
    assert_eq!(
        emit(&mut short).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert!(short.into_inner().is_empty());
}

/// Scenario: hex widths, prefixes, and values beyond signed/float exact ranges pass through JSON.
/// Guarantees: playback matches Rust formatting without rounding or truncating numeric arguments.
#[test]
fn hexadecimal_formatting_matches_rust() {
    for value in [0u64, 42, 0x12345, (1 << 53) + 1, u64::MAX] {
        let mut logger = Logger::new(Vec::new());
        info!("{value:x}|{value:04x}|{value:#04x}|{value:8x}|{value:#08x}|{value:#x}", logger: logger).unwrap();
        let encoded = logger.into_inner();
        assert_eq!(
            replayed(&encoded),
            format!(
                "{value:x}|{value:04x}|{value:#04x}|{value:8x}|{value:#08x}|{value:#x}\nlog.level = INFO\n"
            )
        );
        assert_eq!(records(&encoded)[0].args[0], Argument::Unsigned(value));
    }
}

/// Scenario: malformed or unsupported records follow one valid record.
/// Guarantees: playback stops with a line-numbered error instead of skipping or coercing data.
#[test]
fn invalid_records_fail_loudly() {
    let value = 1u64;
    let mut logger = Logger::new(Vec::new());
    info!("{value:x}", logger: logger).unwrap();
    let events = lines(&logger.into_inner());
    let first = serde_json::to_string(&events[0]).unwrap();
    let base = &events[1];
    let key = format!("log.statement:{}", base.attrs["log.statement"]);
    let base = serde_json::to_value(base).unwrap();
    let mut cases = vec![("not json".into(), "line 2"), (String::new(), "line 2")];
    for (format, args, expected) in [
        ("{v:x}", "[]", "placeholders"),
        ("plain", "[1]", "placeholders"),
        ("{v:x}", r#"["42"]"#, "unsigned integer"),
        ("{v}", "[42]", "string"),
        ("{v:x}", "[-1]", "line 2"),
        ("{v:x}", "[1.5]", "line 2"),
        ("{v:x}", "[18446744073709551616]", "line 2"),
        ("{v:x}", "[null]", "line 2"),
        ("{v:?}", r#"["x"]"#, "unsupported"),
        ("{v:4097x}", "[1]", "width"),
        ("{v:999999999999999999999999x}", "[1]", "width"),
        ("{v:x", "[1]", "unclosed"),
        ("{v{w:x}", "[1]", "nested"),
        ("v}", "[]", "closing brace"),
    ] {
        let mut bad = base.clone();
        bad["defs"][&key]["msg"] = format.into();
        bad["args"] = serde_json::from_str(args).unwrap();
        cases.push((bad.to_string(), expected));
    }
    let mut bad = base.clone();
    bad.as_object_mut().unwrap().remove("args");
    cases.push((bad.to_string(), "args"));
    let mut bad = base;
    bad["attributes"] = serde_json::json!({});
    cases.push((bad.to_string(), "unknown field"));
    for (invalid, expected) in cases {
        let input = format!("{first}\n{invalid}\n");
        let mut output = Vec::new();
        let error = replay(input.as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{invalid}");
        assert!(error.to_string().contains("line 2"), "{error}");
        assert!(error.to_string().contains(expected), "{invalid}: {error}");
        assert_eq!(
            common::body(std::str::from_utf8(&output).unwrap()),
            common::startup_text()
        );
    }
}

struct FailWriter {
    fail_write: bool,
}

impl Write for FailWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        } else {
            Ok(bytes.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

/// Scenario: recording/playback encounters a broken sink, failed flush, or invalid UTF-8 input.
/// Guarantees: every I/O failure is returned to the caller.
#[test]
fn io_errors_are_returned() {
    let mut logger = Logger::new(FailWriter { fail_write: true });
    assert_eq!(
        info!("hello", logger: logger).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(logger.flush().unwrap_err().to_string(), "flush failed");
    let mut logger = Logger::new(Vec::new());
    info!("hello", logger: logger).unwrap();
    let input = logger.into_inner();
    let error = replay(input.as_slice(), FailWriter { fail_write: true }).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    let error = replay(input.as_slice(), FailWriter { fail_write: false }).unwrap_err();
    assert_eq!(error.to_string(), "flush failed");
    let error = replay([0xff, b'\n'].as_slice(), Vec::new()).unwrap_err();
    assert!(error.to_string().contains("line 1"), "{error}");
}

/// Scenario: a caller imports the crate under another name.
/// Guarantees: generated code uses the wrapper's crate and standard-library paths hygienically.
#[test]
fn macro_paths_are_hygienic() {
    use sprintr_prototype as renamed;
    let name = "Ada";
    let value = 42u8;
    let mut logger = renamed::Logger::new(Vec::new());
    renamed::info!("{name:s} {value:04x}", logger: logger).unwrap();
    assert_eq!(
        replayed(&logger.into_inner()),
        "Ada 002a\nlog.level = INFO\n"
    );
}
