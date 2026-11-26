// SPDX-License-Identifier: Apache-2.0

use clap::{Parser, Subcommand};
use sprintr_prototype::{ReplayMode, symbols::ModuleMap};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "sprintr", version, about = "Read sprintr recordings")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stream a JSON Lines recording as formatted text.
    Replay {
        /// Recording to read (default: stdin; use - for stdin explicitly).
        input: Option<PathBuf>,
        /// Print call stacks using symbol packs embedded in the recording.
        #[arg(long, conflicts_with = "module_map")]
        symbols: bool,
        /// Print call stacks with linked addresses from the originating process's module map.
        #[arg(long, value_name = "FILE")]
        module_map: Option<PathBuf>,
    },
}

fn open(path: &Path) -> io::Result<BufReader<File>> {
    File::open(path)
        .map(BufReader::new)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

fn run(
    Command::Replay {
        input,
        symbols,
        module_map,
    }: Command,
) -> io::Result<()> {
    let map: ModuleMap;
    let mode = if let Some(path) = module_map {
        map = serde_json::from_reader(open(&path)?).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
        ReplayMode::ModuleMap(&map)
    } else if symbols {
        ReplayMode::EmbeddedSymbols
    } else {
        ReplayMode::Plain
    };
    let out = io::stdout().lock();
    match input.as_deref() {
        Some(path) if path != Path::new("-") => mode
            .replay(open(path)?, out)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        _ => mode.replay(io::stdin().lock(), out),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sprintr: {e}");
            ExitCode::FAILURE
        }
    }
}
