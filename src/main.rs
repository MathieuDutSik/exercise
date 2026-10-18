use std::env;
use std::fmt::Display;
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

use payments_engine::engine::Engine;
use payments_engine::{process, write_accounts};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let [_, path] = args.as_slice() else {
        eprintln!("usage: payments-engine <transactions.csv>");
        return ExitCode::from(2);
    };

    // The whole state: client accounts and the deposits still disputable.
    let mut engine = Engine::new();

    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => return fail(path, err),
    };
    if let Err(err) = process(&mut BufReader::new(file), &mut engine) {
        return fail(path, err);
    }
    let mut output = BufWriter::new(io::stdout().lock());
    if let Err(err) = write_accounts(&engine, &mut output) {
        return fail("standard output", err);
    }
    ExitCode::SUCCESS
}

/// Reports `err` on standard error and returns the failure exit code.
fn fail(context: &str, err: impl Display) -> ExitCode {
    eprintln!("error: {context}: {err}");
    ExitCode::FAILURE
}
