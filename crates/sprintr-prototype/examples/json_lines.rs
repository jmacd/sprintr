// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::symbols::ModuleMap;
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
    let usage = "usage: json_lines <record|replay> [module-map.json]";
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
            let file = std::fs::File::open(path)?;
            let map: ModuleMap = serde_json::from_reader(io::BufReader::new(file))?;
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
                for &pc in &record.stack {
                    if let Some((module, linked)) = map.locate(pc) {
                        writeln!(
                            out,
                            "      at {pc:#x} ({} @ {linked:#x})",
                            sprintr_message::short_path(&module.path)
                        )?;
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
