use std::env;
use std::fmt::Display;
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

use payments_engine::engine::Engine;
use payments_engine::{process, write_accounts};

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", err.message);
            ExitCode::from(err.code)
        }
    }
}

fn try_main() -> Result<(), MainError> {
    let args: Vec<String> = env::args().collect();
    let [_, path] = args.as_slice() else {
        return Err(MainError::usage());
    };

    // The whole state: client accounts and the deposits still disputable.
    let mut engine = Engine::new();

    let file = File::open(path)?;
    process(&mut BufReader::new(file), &mut engine)?;
    write_accounts(&engine, &mut BufWriter::new(io::stdout().lock()))?;
    Ok(())
}

/// An error ending the program. `code`, the exit status, is the `errno.h`
/// value of the error.
#[derive(Debug)]
struct MainError {
    code: u8,
    message: String,
}

impl MainError {
    /// An error with the OS error code `errno`, or `EIO` without one.
    fn new(errno: Option<libc::c_int>, err: &dyn Display) -> Self {
        MainError {
            code: exit_status(errno.unwrap_or(libc::EIO)),
            message: err.to_string(),
        }
    }

    fn usage() -> Self {
        MainError::new(
            Some(libc::EINVAL),
            &"usage: payments-engine <transactions.csv>",
        )
    }
}

impl From<io::Error> for MainError {
    fn from(err: io::Error) -> Self {
        MainError::new(err.raw_os_error(), &err)
    }
}

impl From<csv::Error> for MainError {
    fn from(err: csv::Error) -> Self {
        let errno = match err.kind() {
            csv::ErrorKind::Io(io_err) => io_err.raw_os_error(),
            _ => None,
        };
        MainError::new(errno, &err)
    }
}

/// `errno` as an exit status. errno values are small positive numbers; one
/// that would not fit in an exit status is reported as `EIO`.
fn exit_status(errno: libc::c_int) -> u8 {
    u8::try_from(errno)
        .ok()
        .filter(|&code| code != 0)
        .unwrap_or(libc::EIO as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_keep_their_errno() {
        let err = io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(MainError::from(err).code, libc::ENOENT as u8);
    }

    #[test]
    fn errors_without_errno_are_eio() {
        let err = MainError::from(io::Error::other("no os code"));
        assert_eq!(err.code, libc::EIO as u8);
        assert_eq!(err.message, "no os code");
    }

    #[test]
    fn csv_errors_keep_their_errno() {
        let csv_err = csv::Error::from(io::Error::from_raw_os_error(libc::EISDIR));
        assert_eq!(MainError::from(csv_err).code, libc::EISDIR as u8);
    }

    #[test]
    fn csv_data_errors_are_eio() {
        let mut reader = csv::Reader::from_reader("n\nx\n".as_bytes());
        let csv_err = reader.deserialize::<u8>().next().unwrap().unwrap_err();
        assert_eq!(MainError::from(csv_err).code, libc::EIO as u8);
    }

    #[test]
    fn out_of_range_errno_is_eio() {
        assert_eq!(exit_status(libc::EPIPE), libc::EPIPE as u8);
        assert_eq!(exit_status(0), libc::EIO as u8);
        assert_eq!(exit_status(-1), libc::EIO as u8);
        assert_eq!(exit_status(256), libc::EIO as u8);
    }
}
