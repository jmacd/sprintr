// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::symbols::{ModuleMap, RecordedSymbols};
use sprintr_prototype::{Message, Renderer, attributes, info, replay};
use std::io::{self, BufRead, Write};

attributes! {
    /// Account name
    account_name: str = "account.name";
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    let modules = arguments.next();
    let usage = "usage: json_lines record [module-map.json] | replay [module-map.json|--symbols]";
    if arguments.next().is_some() {
        return Err(usage.into());
    }
    match (command.as_deref(), modules) {
        (Some("record"), modules) => {
            sprintr_prototype::start()?;
            let value = 42u32;
            for name in ["Ada", "Grace"] {
                info!("hello {}, value {value:04x}", account_name = name);
            }
            if let Some(path) = modules {
                let map = ModuleMap::current_process()?;
                let mut file = io::BufWriter::new(std::fs::File::create(path)?);
                serde_json::to_writer(&mut file, &map)?;
                file.flush()?;
            }
        }
        (Some("replay"), Some(path)) => {
            let map: Option<ModuleMap> = if path == "--symbols" {
                None
            } else {
                let file = std::fs::File::open(path)?;
                Some(serde_json::from_reader(io::BufReader::new(file))?)
            };
            let mut symbols = RecordedSymbols::default();
            let mut renderer = Renderer::default();
            let mut out = io::stdout().lock();
            for (index, line) in io::stdin().lock().lines().enumerate() {
                let record: Message =
                    serde_json::from_str(&line?).map_err(|e| format!("line {}: {e}", index + 1))?;
                writeln!(
                    out,
                    "{}",
                    renderer
                        .render(&record)
                        .map_err(|e| format!("line {}: {e}", index + 1))?
                )?;
                let site = renderer
                    .statement_source(record.attrs["log.statement"])
                    .expect("rendered statement");
                writeln!(out, "  log.stack =\n      {site}")?;
                if map.is_none() {
                    symbols.observe(&record)?;
                }
                for &pc in &record.stack {
                    if let Some((module, linked)) = map.as_ref().and_then(|m| m.locate(pc)) {
                        writeln!(
                            out,
                            "      at {pc:#x} ({} @ {linked:#x})",
                            sprintr_message::short_path(&module.path)
                        )?;
                    } else if let Some(frame) = symbols.format_frame(pc)? {
                        writeln!(out, "      {frame}")?;
                    } else {
                        writeln!(out, "      at <unmapped> ({pc:#x})")?;
                    }
                }
            }
            out.flush()?;
        }
        (Some("replay"), None) => replay(io::stdin().lock(), io::stdout().lock())?,
        _ => return Err(usage.into()),
    }
    Ok(())
}
