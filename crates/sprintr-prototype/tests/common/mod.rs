// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for integration tests.

#![allow(dead_code)]

use sprintr_prototype::{
    Argument, Definition, Logger, Message, RECORD, STATEMENT_PREFIX, StatementTemplate, THREAD,
    replay,
};
use std::io::{self, Write};

pub fn started_logger() -> Logger<Vec<u8>> {
    let mut logger = Logger::new(Vec::new());
    logger.start().unwrap();
    logger
}

/// Reject just the specified write (counting from one); each record is one write.
pub struct FailNth {
    fail: usize,
    pub writes: usize,
    pub output: Vec<u8>,
}

impl FailNth {
    pub fn new(fail: usize) -> Self {
        Self {
            fail,
            writes: 0,
            output: Vec::new(),
        }
    }
}

impl Write for FailNth {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.writes == self.fail {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "sink rejected record",
            ));
        }
        self.output.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn template(record: &Message) -> &StatementTemplate {
    let key = format!("{STATEMENT_PREFIX}{}", record.attrs["log.statement"]);
    let Definition::Statement(template) = &record.defs[&key] else {
        panic!("statement definition")
    };
    template
}

/// Every record in a JSON Lines stream.
pub fn lines(encoded: &[u8]) -> Vec<Message> {
    encoded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

pub fn assert_startup(record: &Message) {
    let thread = record.attrs[THREAD];
    assert!(thread > 0 && record.attrs.contains_key(RECORD));
    assert!(record.defs.contains_key(&format!("log.thread:{thread}")));
    assert_eq!(
        template(record).msg,
        "process {@process.pid} started logging with {@telemetry.sdk.name} {@telemetry.sdk.version}"
    );
    assert_eq!(
        record.args,
        [
            Argument::Unsigned(std::process::id().into()),
            Argument::String("sprintr".into()),
            Argument::String(env!("CARGO_PKG_VERSION").into()),
        ]
    );
}

/// Records after the stream's startup announcement, which is checked.
pub fn records(encoded: &[u8]) -> Vec<Message> {
    let mut records = lines(encoded);
    assert_startup(&records.remove(0));
    records
}

/// The rendered startup announcement.
pub fn startup_text() -> String {
    let (pid, version) = (std::process::id(), env!("CARGO_PKG_VERSION"));
    format!(
        "process {pid} started logging with sprintr {version}\nlog.level = INFO, \
         process.pid = {pid}, telemetry.sdk.name = sprintr, telemetry.sdk.version = {version}\n"
    )
}

/// Ignore the clock prefix in assertions about the message body.
pub fn body(text: &str) -> &str {
    assert_eq!(text.get(23..25), Some("Z "), "{text:?}");
    &text[25..]
}

/// Playback of a stream, without its checked startup announcement.
pub fn replayed(encoded: &[u8]) -> String {
    let mut output = Vec::new();
    replay(encoded, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    let text: String = text
        .split_inclusive('\n')
        .map(|line| {
            if line.get(23..25) == Some("Z ") {
                body(line)
            } else {
                line
            }
        })
        .collect();
    text.strip_prefix(&startup_text())
        .unwrap_or_else(|| panic!("missing startup announcement: {text}"))
        .to_string()
}

/// Make a self-contained reader fixture from a stream with primitive arguments.
pub fn standalone(encoded: &[u8]) -> Message {
    let mut records = lines(encoded);
    let mut last = records.pop().unwrap();
    for record in records {
        for (key, definition) in record.defs {
            if !key.starts_with(STATEMENT_PREFIX) {
                last.defs.entry(key).or_insert(definition);
            }
        }
    }
    last.attrs.remove(RECORD);
    last.attrs.remove(THREAD);
    last.defs.retain(|key, _| !key.starts_with("log.thread:"));
    last
}
