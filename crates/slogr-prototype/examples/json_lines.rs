// SPDX-License-Identifier: Apache-2.0

use slogr_prototype::{info, replay};
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    if arguments.next().is_some() {
        return Err("usage: json_lines <record|replay>".into());
    }
    match command.as_deref() {
        Some("record") => {
            let name = "Ada";
            let value = 42u32;
            info!("hello {name:s}, value {value:04x}")?;
        }
        Some("replay") => replay(io::stdin().lock(), io::stdout().lock())?,
        _ => return Err("usage: json_lines <record|replay>".into()),
    }
    Ok(())
}
