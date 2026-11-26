// SPDX-License-Identifier: Apache-2.0

use crate::symbols::{ModuleMap, RecordedSymbols};
use crate::{Message, Renderer, STATEMENT};
use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

/// Read JSON Lines and write formatted messages without their structured envelope.
pub fn replay(input: impl BufRead, output: impl Write) -> io::Result<()> {
    ReplayMode::Plain.replay(input, output)
}

/// Choose the stack information included after each formatted event.
pub enum ReplayMode<'a> {
    Plain,
    /// Use the GSYM packs and load addresses in the stream being read.
    EmbeddedSymbols,
    /// Relocate PCs for display without doing symbol lookup.
    ModuleMap(&'a ModuleMap),
}

impl ReplayMode<'_> {
    /// Render each complete record immediately. An invalid record stops replay
    /// with its one-based input line; earlier records remain on the output.
    pub fn replay(self, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        let mut renderer = Renderer::default();
        let mut symbols = matches!(self, Self::EmbeddedSymbols).then(RecordedSymbols::default);
        for (index, line) in input.lines().enumerate() {
            let at_line =
                |e: io::Error| io::Error::new(e.kind(), format!("line {}: {e}", index + 1));
            let line = line.map_err(at_line)?;
            let event: Message = serde_json::from_str(&line)
                .map_err(|e| at_line(io::Error::new(io::ErrorKind::InvalidData, e)))?;
            let mut text = renderer
                .render(&event)
                .map_err(|e| at_line(io::Error::new(io::ErrorKind::InvalidData, e)))?;
            if let Some(symbols) = &mut symbols {
                symbols.observe(&event).map_err(at_line)?;
            }
            if !matches!(self, Self::Plain) {
                let site = renderer
                    .statement_source(event.attrs[STATEMENT])
                    .expect("rendered statement");
                write!(text, "\n  log.stack =\n      {site}").expect("write to string");
                for &pc in &event.stack {
                    if let Self::ModuleMap(map) = self
                        && let Some((module, linked)) = map.locate(pc)
                    {
                        write!(
                            text,
                            "\n      at {pc:#x} ({} @ {linked:#x})",
                            sprintr_message::short_path(&module.path)
                        )
                    } else if let Some(symbols) = &symbols
                        && let Some(frame) = symbols.format_frame(pc).map_err(at_line)?
                    {
                        write!(text, "\n      {frame}")
                    } else {
                        write!(text, "\n      at <unmapped> ({pc:#x})")
                    }
                    .expect("write to string");
                }
            }
            writeln!(output, "{text}")?;
        }
        output.flush()
    }
}
