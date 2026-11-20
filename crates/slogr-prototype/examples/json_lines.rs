// SPDX-License-Identifier: Apache-2.0

use slogr_prototype::{info, register_attribute, replay};
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    if arguments.next().is_some() {
        return Err("usage: json_lines <record|replay>".into());
    }
    match command.as_deref() {
        Some("record") => {
            let acct_name = register_attribute("account.name", "Account name")?;
            let name = "Ada";
            let value = 42u32;
            info!("hello {}, value {value:04x}", acct_name = name)?;
        }
        Some("replay") => replay(io::stdin().lock(), io::stdout().lock())?,
        _ => return Err("usage: json_lines <record|replay>".into()),
    }
    Ok(())
}
