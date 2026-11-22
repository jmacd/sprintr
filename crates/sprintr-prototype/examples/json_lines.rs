// SPDX-License-Identifier: Apache-2.0

use sprintr_prototype::{attributes, info, replay};
use std::io;

attributes! {
    /// Account name
    account_name: str = "account.name";
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    if arguments.next().is_some() {
        return Err("usage: json_lines <record|replay>".into());
    }
    match command.as_deref() {
        Some("record") => {
            let value = 42u32;
            for name in ["Ada", "Grace"] {
                info!("hello {}, value {value:04x}", account_name = name)?;
            }
        }
        Some("replay") => replay(io::stdin().lock(), io::stdout().lock())?,
        _ => return Err("usage: json_lines <record|replay>".into()),
    }
    Ok(())
}
