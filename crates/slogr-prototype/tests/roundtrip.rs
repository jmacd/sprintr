// SPDX-License-Identifier: Apache-2.0

use slogr_prototype::buffer::{HeapBuffer, RecordBuffer, StackBuffer};
use slogr_prototype::{
    Argument, Definition, Logger, Message, StatementTemplate, info, message, replay,
};
use std::cell::Cell;
use std::io::{self, Write};

fn template(record: &Message) -> &StatementTemplate {
    let key = format!("log.statement:{}", record.attrs["log.statement"]);
    let Definition::Statement(template) = &record.defs[&key] else {
        panic!("statement definition")
    };
    template
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
    let records: Vec<Message> = encoded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(template(&records[0]), template(&records[1]));
    assert_eq!(template(&records[0]).msg, "hello {name}, value {value:04x}");
    assert_eq!(
        records[0].args,
        [Argument::String("Ada".into()), Argument::Unsigned(42)]
    );
    assert_eq!(records[0].args, records[1].args);
    let mut output = Vec::new();
    replay(encoded.as_slice(), &mut output).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "hello Ada, value 002a\nhello Ada, value 002a\n"
    );
    assert_eq!(name, "Ada");
}

/// Scenario: mixed implicit and explicit arguments include an AsRef-only value and a side effect.
/// Guarantees: capture borrows strings, snapshots eagerly, and evaluates each expression once.
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
    let record = message!("hello {name:s}, value {:04x}", next());
    name.0.clear();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        record.args,
        [Argument::String("Ada".into()), Argument::Unsigned(42)]
    );
    assert_eq!(record.render().unwrap(), "hello Ada, value 002a");
}

/// Scenario: raw literals, escaped braces, repeated captures, and JSON-sensitive string data are logged.
/// Guarantees: data stays on one JSON line and is not interpreted as another formatting template.
#[test]
fn escaped_literals_and_string_data() {
    let text = "quoted \"text\"\n\\{braces}\t\u{03bb}";
    let record = message!(r"{{{text:s}}} {text:s}");
    let mut logger = Logger::new(Vec::new());
    logger.write(&record).unwrap();
    let encoded = logger.into_inner();
    assert_eq!(encoded.iter().filter(|byte| **byte == b'\n').count(), 1);
    let decoded: Message = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, record);
    assert_eq!(decoded.render().unwrap(), format!("{{{text}}} {text}"));
    assert_eq!(message!("{{literal}}").render().unwrap(), "{literal}");
    assert_eq!(message!("").render().unwrap(), "");
}

/// Scenario: direct encoding includes escaped format text, Unicode, braces, and a temporary argument.
/// Guarantees: it agrees with owned-message encoding and each temporary is evaluated once.
#[test]
fn direct_encoding_matches_owned_encoding() {
    let text = "quoted \"text\"\n\\{braces}\t\u{03bb}";
    let value = u64::MAX;
    let mut logger = Logger::new(Vec::new());
    info!("line\n{{{text:s}}} {value:#04x}", logger: logger).unwrap();
    let owned = message!("line\n{{{text:s}}} {value:#04x}");
    let direct: Message = serde_json::from_slice(&logger.into_inner()).unwrap();
    assert_eq!(direct.args, owned.args);
    assert_eq!(template(&direct), template(&owned));
    assert_eq!(direct.render().unwrap(), owned.render().unwrap());

    let calls = Cell::new(0);
    let temporary = || {
        calls.set(calls.get() + 1);
        String::from("temporary")
    };
    let mut logger = Logger::new(Vec::new());
    info!("hello {:s}", logger: logger, temporary()).unwrap();
    let mut output = Vec::new();
    replay(logger.into_inner().as_slice(), &mut output).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(output, b"hello temporary\n");
}

/// Scenario: escaped argument data exceeds a bounded buffer between two successful writes.
/// Guarantees: neither storage strategy emits partial JSON or loses subsequent records.
#[test]
fn oversized_records_roll_back_and_allow_reuse() {
    fn check(buffer: impl RecordBuffer) {
        let mut logger = Logger::with_buffer(Vec::new(), buffer);
        info!("before", logger: logger).unwrap();
        let text = "\n".repeat(140);
        assert_eq!(
            info!("{text:s}", logger: logger).unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        info!("after", logger: logger).unwrap();
        let mut output = Vec::new();
        replay(logger.into_inner().as_slice(), &mut output).unwrap();
        assert_eq!(output, b"before\nafter\n");
    }
    check(HeapBuffer::with_capacity_and_limit(16, 256));
    check(StackBuffer::<256>::new());
}

/// Scenario: an encoded record exactly fills storage, or lacks room for its final newline.
/// Guarantees: limits apply to the complete wire record, including framing.
#[test]
fn buffer_limit_includes_record_framing() {
    fn emit(logger: &mut Logger<Vec<u8>>) -> io::Result<()> {
        info!("hello", logger: logger)
    }
    let mut sample = Logger::new(Vec::new());
    emit(&mut sample).unwrap();
    let line = sample.into_inner();
    let mut exact = Logger::with_buffer(
        Vec::new(),
        HeapBuffer::with_capacity_and_limit(line.len(), line.len()),
    );
    emit(&mut exact).unwrap();
    assert_eq!(exact.into_inner(), line);

    let mut short = Logger::with_buffer(
        Vec::new(),
        HeapBuffer::with_capacity_and_limit(line.len(), line.len() - 1),
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
        let record =
            message!("{value:x}|{value:04x}|{value:#04x}|{value:8x}|{value:#08x}|{value:#x}");
        let json = serde_json::to_string(&record).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded.render().unwrap(),
            format!("{value:x}|{value:04x}|{value:#04x}|{value:8x}|{value:#08x}|{value:#x}")
        );
        assert_eq!(decoded.args[0], Argument::Unsigned(value));
    }
}

/// Scenario: malformed or unsupported records follow one valid record.
/// Guarantees: playback stops with a line-numbered error instead of skipping or coercing data.
#[test]
fn invalid_records_fail_loudly() {
    let value = 1u64;
    let base = message!("{value:x}");
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
    let first = serde_json::to_string(&message!("first")).unwrap();
    for (invalid, expected) in cases {
        let input = format!("{first}\n{invalid}\n");
        let mut output = Vec::new();
        let error = replay(input.as_bytes(), &mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{invalid}");
        assert!(error.to_string().contains("line 2"), "{error}");
        assert!(error.to_string().contains(expected), "{invalid}: {error}");
        assert_eq!(output, b"first\n");
    }
}

/// Scenario: a manually constructed record has arguments incompatible with its stored format.
/// Guarantees: the writer rejects it before producing a partial JSON line.
#[test]
fn writer_validates_before_output() {
    let mut logger = Logger::new(Vec::new());
    let value = 1u32;
    let mut record = message!("{value:x}");
    record.args[0] = Argument::String("not numeric".into());
    assert_eq!(
        logger.write(&record).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(logger.into_inner().is_empty());
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
    let input = serde_json::to_vec(&message!("hello")).unwrap();
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
    use slogr_prototype as renamed;
    let name = "Ada";
    let value = 42u8;
    let record = renamed::message!("{name:s} {value:04x}");
    assert_eq!(record.render().unwrap(), "Ada 002a");
}
