// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::symbols::ModuleMap;
use sprintr_prototype::{ReplayMode, attributes, info};
use std::io::{self, Write};

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
        (Some("replay"), modules) => {
            let map: ModuleMap;
            let mode = match modules.as_deref() {
                None => ReplayMode::Plain,
                Some("--symbols") => ReplayMode::EmbeddedSymbols,
                Some(path) => {
                    map = serde_json::from_reader(io::BufReader::new(std::fs::File::open(path)?))?;
                    ReplayMode::ModuleMap(&map)
                }
            };
            mode.replay(io::stdin().lock(), io::stdout().lock())?;
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}
