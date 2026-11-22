// SPDX-License-Identifier: Apache-2.0

//! A tour of recording and replay with sprintr.
//!
//! ```sh
//! cargo run --quiet -p sprintr-prototype --example tour
//! ```
//!
//! Each JSON record is followed immediately by its indented rendering.

use serde::Serialize;
use sprintr_prototype::buffer::StackBuffer;
use sprintr_prototype::symbols::LocalSymbols;
use sprintr_prototype::{
    Logger, Message, Renderer, attributes, entities, error, info, message, replay, warn,
};
use std::io::{self, BufWriter, Write};

// Attributes are declared once, with doc comments as their recorded
// descriptions. Handles are lowercase so that log calls read like assignments.
attributes! {
    /// Account name
    pub account_name: str = "account.name";
    /// Retry attempt
    pub retry_count: u64 = "retry.count";
    /// Request identifier
    pub request_id: u64 = "request.id";
}

// Entity relations describe object values. Statically named values are the
// unusual case; `log.level` (INFO, WARN, ERROR) is declared the same way in
// `sprintr_prototype::schema`. An anonymous relation instead creates a
// fresh identity for each application object.
entities! {
    /// Deployment environment
    pub environment = "deployment.environment" {
        /// Production traffic
        PRODUCTION,
        /// Pre-production testing
        STAGING,
    }
    /// A user session; its values have identities, not names
    #[anonymous]
    pub session = "session";
}

#[derive(Serialize)]
struct Login<'a> {
    account: &'a str,
    method: &'a str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A caller-owned logger over any `io::Write`. Its first record is the
    // startup announcement.
    let mut logger = Logger::new(Vec::new());

    // Both arguments point to the same definition. The sequence before `#`
    // matches the opened record's `seq`. The resumed record
    // has its own sequence on the same `log.thread`.
    let login = session.new(Login {
        account: "Ada",
        method: "passkey",
    })?;
    info!("opened {}", logger: logger, login)?;
    info!("resumed {}", logger: logger, login)?;

    // Plain formatting: implicit captures name local variables, and explicit
    // positional expressions fill `{}` placeholders. Strings take `:s`;
    // unsigned integers take `:x` with `#`, zero padding, and width.
    let name = "Ada";
    let value = 42u32;
    info!("hello {name:s}, value {value:04x}", logger: logger)?;
    info!("hello {:s}, value {:#x}", logger: logger, name, value)?;
    info!("{{literal braces}} around {name:s}", logger: logger)?;

    // Attributes: `handle = expression` associates a placeholder with a
    // declared attribute. The stored placeholder is `{@account.name}`.
    // Ada's record defines the anonymous log.statement; Grace's refers to
    // the same statement by ID without repeating its template.
    for name in ["Ada", "Grace"] {
        info!("signed in {}", logger: logger, account_name = name)?;
    }

    // Levels are static named entities baked into each statement. Unsigned
    // attributes render `{}` in decimal or `{:x}` in hexadecimal.
    warn!(
        "attempt {} failed for {}",
        logger: logger,
        retry_count = 2u8,
        account_name = name,
    )?;
    error!(
        "request {:016x} gave up after {} attempts",
        logger: logger,
        request_id = 0xdead_beef_u64,
        retry_count = 3u8,
    )?;

    // Bounded storage fails loudly and atomically; the logger remains usable.
    // It must fit the startup announcement, for which 2 KB suffices.
    let mut bounded = Logger::with_buffer(io::sink(), StackBuffer::<2048>::new());
    let long = "x".repeat(4096);
    let reason = info!("{long:s}", logger: bounded).unwrap_err().to_string();
    info!("short records still fit", logger: bounded)?;
    error!("bounded logger rejected a record: {reason:s}", logger: logger)?;

    // `message!` captures an owned, unlevelled snapshot that carries all of
    // its definitions, renders independently, and can be written later.
    let snapshot = message!("snapshot of {} at {value:#x}", account_name = name);
    assert!(
        snapshot
            .render()?
            .ends_with("Z snapshot of Ada at 0x2a\naccount.name = Ada")
    );
    logger.write(&snapshot)?;

    logger.flush()?;
    let encoded = logger.into_inner();

    // A reader needs only the recorded JSON. `Renderer` retains definitions
    // across records; `replay` does the same for a whole stream.
    let mut out = BufWriter::new(io::stdout().lock());
    let mut renderer = Renderer::default();
    let mut rendered = Vec::new();
    let mut symbols = LocalSymbols::current_process()?;
    for line in std::str::from_utf8(&encoded)?.lines() {
        let record: Message = serde_json::from_str(line)?;
        let text = renderer.render(&record)?;
        writeln!(out, "{line}")?;
        for line in text.lines() {
            writeln!(out, "    {line}")?;
        }
        let site = renderer
            .statement_source(record.attrs["log.statement"])
            .expect("rendered statement");
        writeln!(out, "    log.stack =\n        {site}")?;
        // Only callers need symbolization; frame zero came from the compiler.
        for &pc in &record.stack {
            writeln!(out, "        {}", symbols.format_frame(pc))?;
        }
        writeln!(out)?;
        out.flush()?;
        writeln!(rendered, "{text}")?;
    }
    let mut replayed = Vec::new();
    replay(encoded.as_slice(), &mut replayed)?;
    assert_eq!(replayed, rendered);

    // Declarations are ordinary statics with their metadata.
    let staging = &environment::STAGING;
    writeln!(out, "{}: {}", environment.name(), environment.description())?;
    writeln!(out, "  {}: {}", staging.name(), staging.description())?;
    out.flush()?;
    Ok(())
}
