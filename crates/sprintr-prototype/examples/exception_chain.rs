// SPDX-License-Identifier: Apache-2.0

use sprintr_message::{Argument, ExceptionTarget};
use sprintr_prototype::symbols::RecordedSymbols;
use sprintr_prototype::{Message, Renderer, StructuredError, exception, warn};
use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
enum PipelineError {
    #[error(transparent)]
    UnknownOutput(StructuredError),
    #[error(transparent)]
    InvalidConfig(StructuredError),
}

fn route(port: &str) -> PipelineError {
    exception!(PipelineError::UnknownOutput, "unknown output {port:s}")
}

fn load(path: &str) -> PipelineError {
    match route("metrics") {
        PipelineError::UnknownOutput(inner) => {
            exception!(
                PipelineError::InvalidConfig,
                "could not load {path:s}",
                cause = &inner
            )
        }
        other => other,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("record") => {
            sprintr_prototype::start()?;
            let err = load("agent.yaml");
            eprintln!("{err}"); // Application Display, independent of playback.
            if let PipelineError::InvalidConfig(problem) = err {
                warn!("startup failed: {problem}"); // Only a reference on the wire.
            }
        }
        Some("replay") => {
            let mut reader = Renderer::default();
            let mut symbols = RecordedSymbols::default();
            for event in
                serde_json::Deserializer::from_reader(io::stdin().lock()).into_iter::<Message>()
            {
                let event = event?;
                reader.render(&event)?;
                symbols.observe(&event)?;
                // Other single-reference arguments can name DFE entities or spans.
                if let [Argument::Reference { target }] = event.args.as_slice()
                    && let Ok(exception) = ExceptionTarget::parse(target)
                {
                    let origin = exception
                        .origin
                        .ok_or("reported exception was not recorded")?;
                    let chain = reader
                        .try_exception_chain(origin, |pc| symbols.format_frame(pc))?
                        .ok_or("missing exception record")?;
                    println!("{chain}");
                }
            }
        }
        _ => return Err("usage: exception_chain <record|replay>".into()),
    }
    Ok(())
}
