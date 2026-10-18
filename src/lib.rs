//! A toy payments engine: reads transactions from CSV, applies them to
//! client accounts and writes the final balances as CSV.

pub mod amount;
pub mod engine;

use std::io::{Read, Write};

use serde::Deserialize;

use amount::Amount;
use engine::{ClientId, Engine, Transaction, TxId};

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

/// One input row. `amount` is absent (or empty) for disputes, resolves and
/// chargebacks.
#[derive(Debug, Deserialize)]
struct Row<'a> {
    #[serde(rename = "type")]
    kind: Kind,
    client: ClientId,
    tx: TxId,
    amount: Option<&'a str>,
}

impl Row<'_> {
    /// The transaction this row describes, or `None` when a deposit or a
    /// withdrawal has a missing or invalid amount.
    fn to_transaction(&self) -> Option<Transaction> {
        let Row { client, tx, .. } = *self;
        let amount = || self.amount?.parse::<Amount>().ok();
        Some(match self.kind {
            Kind::Deposit => Transaction::Deposit {
                client,
                tx,
                amount: amount()?,
            },
            Kind::Withdrawal => Transaction::Withdrawal {
                client,
                tx,
                amount: amount()?,
            },
            Kind::Dispute => Transaction::Dispute { client, tx },
            Kind::Resolve => Transaction::Resolve { client, tx },
            Kind::Chargeback => Transaction::Chargeback { client, tx },
        })
    }
}

/// Applies every transaction of the CSV `input` to `engine`, in order.
///
/// Rows that cannot be parsed and transactions the engine rejects are
/// skipped: they are treated as errors on the partner's side. Only failing
/// to read the input is reported as an error.
pub fn process(input: &mut dyn Read, engine: &mut Engine) -> csv::Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(input);
    let headers = reader.byte_headers()?.clone();
    // A single record buffer is reused for every row, so reading allocates
    // nothing per transaction.
    let mut record = csv::ByteRecord::new();
    while reader.read_byte_record(&mut record)? {
        let tx = record
            .deserialize::<Row>(Some(&headers))
            .ok()
            .and_then(|row| row.to_transaction());
        if let Some(tx) = tx {
            // A rejected transaction leaves the engine unchanged.
            let _ = engine.apply(tx);
        }
    }
    Ok(())
}

/// Writes the balances of every account as CSV, sorted by client id.
pub fn write_accounts(engine: &Engine, output: &mut dyn Write) -> std::io::Result<()> {
    let mut accounts: Vec<_> = engine.accounts().collect();
    accounts.sort_unstable_by_key(|&(client, _)| client);
    writeln!(output, "client,available,held,total,locked")?;
    for (client, a) in accounts {
        writeln!(
            output,
            "{client},{},{},{},{}",
            a.available,
            a.held,
            a.total(),
            a.locked
        )?;
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn engine_from(input: &str) -> Engine {
        let mut engine = Engine::new();
        process(&mut input.as_bytes(), &mut engine).unwrap();
        engine
    }

    fn run_str(input: &str) -> String {
        let mut out = Vec::new();
        write_accounts(&engine_from(input), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn example_from_spec() {
        let input = "type, client, tx, amount
deposit, 1, 1, 1.0
deposit, 2, 2, 2.0
deposit, 1, 3, 2.0
withdrawal, 1, 4, 1.5
withdrawal, 2, 5, 3.0
";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n1,1.5,0,1.5,false\n2,2,0,2,false\n"
        );
    }

    #[test]
    fn disputes_without_amount_column() {
        let input = "type,client,tx,amount
deposit,1,1,10
deposit,1,2,5.5
dispute,1,2
deposit,2,3,1
dispute,2,3,
chargeback,2,3
resolve,1,2
dispute,1,1,
";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n1,5.5,10,15.5,false\n2,0,0,0,true\n"
        );
    }

    #[test]
    fn dispute_after_withdrawal_gives_negative_funds() {
        let input = "type,client,tx,amount
deposit,1,1,10
withdrawal,1,2,10
dispute,1,1,
withdrawal,1,3,0.0001
";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n1,-10,10,0,false\n"
        );
    }

    #[test]
    fn invalid_rows_are_skipped() {
        let input = "type,client,tx,amount
deposit,1,1,1
deposit,1,2
withdrawal,1,3,
deposit,1,4,abc
deposit,1,5,1.00001
deposit,1,6,-1
transfer,1,7,1
deposit,70000,8,1
deposit,x,9,1
deposit,1,-1,1
Deposit,1,10,1
deposit,1,11,0.25
";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n1,1.25,0,1.25,false\n"
        );
    }

    #[test]
    fn whitespace_is_accepted() {
        let input = "  type ,client,  tx,amount  \n  deposit  ,  3 ,  1  ,  2.5  \n\twithdrawal\t,3,2,\t0.5\n";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n3,2,0,2,false\n"
        );
    }

    #[test]
    fn columns_are_matched_by_header() {
        // Columns may come in any order, and unknown columns are ignored.
        let input = "amount,note,tx,client,type\n7.5,hello,1,4,deposit\n";
        assert_eq!(
            run_str(input),
            "client,available,held,total,locked\n4,7.5,0,7.5,false\n"
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(run_str(""), "client,available,held,total,locked\n");
        assert_eq!(
            run_str("type,client,tx,amount\n"),
            "client,available,held,total,locked\n"
        );
    }

    #[test]
    fn rows_to_transactions() {
        let row = |kind, amount| Row {
            kind,
            client: 4,
            tx: 2,
            amount,
        };
        let one = Amount::from_raw(10_000);
        assert_eq!(
            row(Kind::Deposit, Some("1")).to_transaction(),
            Some(Transaction::Deposit {
                client: 4,
                tx: 2,
                amount: one
            })
        );
        assert_eq!(
            row(Kind::Withdrawal, Some("1")).to_transaction(),
            Some(Transaction::Withdrawal {
                client: 4,
                tx: 2,
                amount: one
            })
        );
        assert_eq!(row(Kind::Deposit, None).to_transaction(), None);
        assert_eq!(row(Kind::Withdrawal, Some("x")).to_transaction(), None);
        // An amount on a dispute, resolve or chargeback is ignored.
        assert_eq!(
            row(Kind::Dispute, Some("x")).to_transaction(),
            Some(Transaction::Dispute { client: 4, tx: 2 })
        );
        assert_eq!(
            row(Kind::Resolve, None).to_transaction(),
            Some(Transaction::Resolve { client: 4, tx: 2 })
        );
        assert_eq!(
            row(Kind::Chargeback, None).to_transaction(),
            Some(Transaction::Chargeback { client: 4, tx: 2 })
        );
    }

    /// A reader that fails on its first read.
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failure"))
        }
    }

    /// A reader that yields a valid header, then fails.
    struct FailingAfterHeader(bool);

    impl Read for FailingAfterHeader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.0, true) {
                return Err(io::Error::other("read failure"));
            }
            let header = b"type,client,tx,amount\n";
            buf[..header.len()].copy_from_slice(header);
            Ok(header.len())
        }
    }

    /// A writer that accepts `writes_left` writes, then fails every write
    /// and flush.
    struct FailingWriter {
        writes_left: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.writes_left == 0 {
                return Err(io::Error::other("write failure"));
            }
            self.writes_left -= 1;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failure"))
        }
    }

    #[test]
    fn read_errors_are_reported() {
        let mut engine = Engine::new();
        let err = process(&mut FailingReader, &mut engine).unwrap_err();
        assert!(err.to_string().contains("read failure"), "{err}");
        let err = process(&mut FailingAfterHeader(false), &mut engine).unwrap_err();
        assert!(err.to_string().contains("read failure"), "{err}");
    }

    #[test]
    fn write_errors_are_reported() {
        let engine = engine_from("type,client,tx,amount\ndeposit,1,1,1\n");
        // `writeln!` performs several writes per line: fail on the header,
        // on the first account row, and finally on the flush.
        for writes_left in [0, 1, usize::MAX] {
            let err = write_accounts(&engine, &mut FailingWriter { writes_left }).unwrap_err();
            assert!(err.to_string().contains("failure"), "{err}");
        }
    }
}
