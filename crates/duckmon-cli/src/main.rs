//! `duckmon`: query, write, follow and compact a duckmon store from the shell.

mod compact;
mod emit;
mod output;
mod query;
mod tail;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Query, write, follow and compact a duckmon telemetry store.
///
/// Exit codes: 0 success, 1 runtime failure, 2 bad command line.
#[derive(Debug, Parser)]
#[command(name = "duckmon", version)]
struct Cli {
    /// Store directory. Falls back to the DUCKMON_DIR environment variable.
    #[arg(long, env = "DUCKMON_DIR", global = true)]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run SQL and print the result. The view `events` holds every event.
    ///
    /// When the SQL holds several statements separated by `;`, all of them run
    /// and only the result of the last one is printed.
    Query {
        #[arg(long, value_enum, default_value_t = output::Format::Table)]
        format: output::Format,
        /// The SQL to run.
        sql: String,
    },
    /// Write one event. Creates the store directory when it is missing.
    Emit(emit::EmitArgs),
    /// Print new events as they arrive, until stopped with Ctrl-C.
    ///
    /// Files that exist at startup are not printed. Each new file is printed once,
    /// as long as tail looks at least once per settle time of `compact`.
    /// A new file that cannot be read holds back all later events, with a
    /// warning at each look, until the file is removed.
    Tail(tail::TailArgs),
    /// Merge the small files of past days and delete old days.
    Compact(compact::CompactArgs),
}

/// Why a command stopped early.
#[derive(Debug)]
pub enum Failure {
    /// A value on the command line is wrong. Exit code 2.
    BadInput(String),
    /// Something failed while running. Exit code 1.
    Runtime(String),
    /// The reader of stdout went away. Exit code 0, prints nothing.
    OutputClosed,
}

impl From<duckmon::Error> for Failure {
    fn from(err: duckmon::Error) -> Self {
        Failure::Runtime(err.to_string())
    }
}

impl From<io::Error> for Failure {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::BrokenPipe {
            Failure::OutputClosed
        } else {
            Failure::Runtime(format!("file error: {err}"))
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) | Err(Failure::OutputClosed) => ExitCode::SUCCESS,
        Err(Failure::BadInput(message)) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(message)) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Failure> {
    let Some(dir) = cli.dir else {
        return Err(Failure::BadInput("no store directory: pass --dir or set DUCKMON_DIR".to_string()));
    };
    match cli.command {
        Command::Query { format, sql } => {
            require_directory(&dir)?;
            query::run(&dir, &sql, format)
        }
        Command::Emit(args) => emit::run(&dir, args),
        Command::Tail(args) => {
            require_directory(&dir)?;
            tail::run(&dir, &args)
        }
        Command::Compact(args) => {
            require_directory(&dir)?;
            compact::run(&dir, &args)
        }
    }
}

/// Reading commands never create a store, so a mistyped path fails loudly.
fn require_directory(dir: &Path) -> Result<(), Failure> {
    if dir.is_dir() {
        Ok(())
    } else {
        Err(Failure::Runtime(format!("store directory {} does not exist", dir.display())))
    }
}
