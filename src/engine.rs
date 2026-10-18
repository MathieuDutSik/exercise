//! The transactions engine: client accounts and the deposits that can still
//! be disputed.
//!
//! Every operation is atomic: it is validated first and the state is only
//! mutated once nothing can fail any more. A rejected operation leaves the
//! engine exactly as it was and reports why through [`TxError`].

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use crate::amount::Amount;

pub type ClientId = u16;
pub type TxId = u32;

/// A transaction, as read from one input row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transaction {
    Deposit {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Withdrawal {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Dispute {
        client: ClientId,
        tx: TxId,
    },
    Resolve {
        client: ClientId,
        tx: TxId,
    },
    Chargeback {
        client: ClientId,
        tx: TxId,
    },
}

/// Why a transaction was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxError {
    /// The account was frozen by an earlier chargeback.
    AccountLocked,
    /// A deposit reuses the id of a deposit that is still tracked.
    DuplicateTx,
    /// A withdrawal needs more than the available funds, or the available
    /// funds are negative.
    InsufficientFunds,
    /// A balance would go beyond the representable range.
    Overflow,
    /// The referenced deposit does not exist (anymore) for this client.
    UnknownTx,
    /// The referenced deposit is already under dispute.
    AlreadyDisputed,
    /// The referenced deposit is not under dispute.
    NotDisputed,
}

impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            TxError::AccountLocked => "account is locked",
            TxError::DuplicateTx => "duplicate transaction id",
            TxError::InsufficientFunds => "insufficient available funds",
            TxError::Overflow => "balance overflow",
            TxError::UnknownTx => "unknown transaction",
            TxError::AlreadyDisputed => "transaction already disputed",
            TxError::NotDisputed => "transaction not under dispute",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for TxError {}

/// The balances of one client. `total` is derived as `available + held`,
/// so the three values can never disagree. `held` is never negative, but
/// `available` (and so `total`) is when a deposit already withdrawn is
/// disputed: the client is then in debt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Account {
    pub available: Amount,
    pub held: Amount,
    pub locked: bool,
}

impl Account {
    /// `available + held`. Cannot overflow: deposits are only accepted when
    /// the new total fits, disputes and resolves leave it unchanged, and
    /// withdrawals and chargebacks only lower it to `available + held` with
    /// `held >= 0`.
    pub fn total(&self) -> Amount {
        self.available
            .checked_add(self.held)
            .expect("account total is kept within range")
    }
}

/// A deposit that can still be disputed. Withdrawals are never kept: they
/// cannot be disputed. A deposit is forgotten once its dispute is resolved
/// or charged back, so it can be disputed at most once.
#[derive(Debug, Clone, Copy)]
struct TrackedDeposit {
    client: ClientId,
    amount: Amount,
    disputed: bool,
}

#[derive(Debug, Default)]
pub struct Engine {
    accounts: HashMap<ClientId, Account>,
    deposits: HashMap<TxId, TrackedDeposit>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one transaction. On error the engine is left unchanged.
    pub fn apply(&mut self, tx: Transaction) -> Result<(), TxError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => self.deposit(client, tx, amount),
            Transaction::Withdrawal { client, amount, .. } => self.withdraw(client, amount),
            Transaction::Dispute { client, tx } => self.dispute(client, tx),
            Transaction::Resolve { client, tx } => self.resolve(client, tx),
            Transaction::Chargeback { client, tx } => self.chargeback(client, tx),
        }
    }

    /// The account of `client`, if it ever received a successful deposit.
    pub fn account(&self, client: ClientId) -> Option<&Account> {
        self.accounts.get(&client)
    }

    /// All accounts, in no particular order.
    pub fn accounts(&self) -> impl Iterator<Item = (ClientId, &Account)> {
        self.accounts.iter().map(|(&id, acc)| (id, acc))
    }

    fn deposit(&mut self, client: ClientId, tx: TxId, amount: Amount) -> Result<(), TxError> {
        let current = self.accounts.get(&client).copied().unwrap_or_default();
        if current.locked {
            return Err(TxError::AccountLocked);
        }
        let Entry::Vacant(slot) = self.deposits.entry(tx) else {
            return Err(TxError::DuplicateTx);
        };
        // Checking the total also covers `available`, which is never larger.
        current
            .total()
            .checked_add(amount)
            .ok_or(TxError::Overflow)?;
        let available = current.available.checked_add(amount).expect("total fits");

        slot.insert(TrackedDeposit {
            client,
            amount,
            disputed: false,
        });
        self.accounts.insert(
            client,
            Account {
                available,
                ..current
            },
        );
        Ok(())
    }

    fn withdraw(&mut self, client: ClientId, amount: Amount) -> Result<(), TxError> {
        // A client without an account has no funds: report that rather than
        // creating an empty account for a failed operation.
        let account = self
            .accounts
            .get_mut(&client)
            .ok_or(TxError::InsufficientFunds)?;
        if account.locked {
            return Err(TxError::AccountLocked);
        }
        // Funds must remain non-negative: a client in debt cannot withdraw.
        account.available = account
            .available
            .checked_sub(amount)
            .filter(|available| !available.is_negative())
            .ok_or(TxError::InsufficientFunds)?;
        Ok(())
    }

    fn dispute(&mut self, client: ClientId, tx: TxId) -> Result<(), TxError> {
        let (account, deposit) = self.disputable(client, tx)?;
        if deposit.disputed {
            return Err(TxError::AlreadyDisputed);
        }
        // If the deposited funds were already withdrawn, the available
        // funds become negative: the client is in debt.
        let available = account.available.checked_sub(deposit.amount);
        let held = account.held.checked_add(deposit.amount);
        let (Some(available), Some(held)) = (available, held) else {
            return Err(TxError::Overflow);
        };
        account.available = available;
        account.held = held;
        deposit.disputed = true;
        Ok(())
    }

    fn resolve(&mut self, client: ClientId, tx: TxId) -> Result<(), TxError> {
        let amount = self.settle(client, tx)?;
        let account = self.accounts.get_mut(&client).expect("checked by settle");
        account.held = account
            .held
            .checked_sub(amount)
            .expect("disputed funds are held");
        account.available = account
            .available
            .checked_add(amount)
            .expect("total is unchanged");
        Ok(())
    }

    fn chargeback(&mut self, client: ClientId, tx: TxId) -> Result<(), TxError> {
        let amount = self.settle(client, tx)?;
        let account = self.accounts.get_mut(&client).expect("checked by settle");
        account.held = account
            .held
            .checked_sub(amount)
            .expect("disputed funds are held");
        account.locked = true;
        Ok(())
    }

    /// Ends the dispute on deposit `tx` of `client`: checks it is under
    /// dispute, forgets it and returns its amount.
    fn settle(&mut self, client: ClientId, tx: TxId) -> Result<Amount, TxError> {
        let (_, deposit) = self.disputable(client, tx)?;
        if !deposit.disputed {
            return Err(TxError::NotDisputed);
        }
        let amount = deposit.amount;
        self.deposits.remove(&tx);
        Ok(amount)
    }

    /// The account of `client` and its tracked deposit `tx`, provided the
    /// account is not locked. A deposit made by another client is treated as
    /// unknown.
    fn disputable(
        &mut self,
        client: ClientId,
        tx: TxId,
    ) -> Result<(&mut Account, &mut TrackedDeposit), TxError> {
        let deposit = self
            .deposits
            .get_mut(&tx)
            .filter(|d| d.client == client)
            .ok_or(TxError::UnknownTx)?;
        let account = self
            .accounts
            .get_mut(&client)
            .expect("a deposit implies an account");
        if account.locked {
            return Err(TxError::AccountLocked);
        }
        Ok((account, deposit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Transaction::*;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn engine_with(txs: &[Transaction]) -> Engine {
        let mut engine = Engine::new();
        for &tx in txs {
            engine.apply(tx).unwrap();
        }
        engine
    }

    fn balances(engine: &Engine, client: ClientId) -> (String, String, String, bool) {
        let a = engine.account(client).unwrap();
        (
            a.available.to_string(),
            a.held.to_string(),
            a.total().to_string(),
            a.locked,
        )
    }

    fn b(available: &str, held: &str, total: &str, locked: bool) -> (String, String, String, bool) {
        (available.into(), held.into(), total.into(), locked)
    }

    /// Applies `tx`, expects `err`, and checks the engine did not change.
    fn assert_rejected(engine: &mut Engine, tx: Transaction, err: TxError) {
        let accounts_before: Vec<_> = sorted_accounts(engine);
        let deposits_before = format!("{:?}", sorted_deposits(engine));
        assert_eq!(engine.apply(tx), Err(err));
        assert_eq!(sorted_accounts(engine), accounts_before);
        assert_eq!(format!("{:?}", sorted_deposits(engine)), deposits_before);
    }

    fn sorted_accounts(engine: &Engine) -> Vec<(ClientId, Account)> {
        let mut v: Vec<_> = engine.accounts().map(|(id, a)| (id, *a)).collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }

    fn sorted_deposits(engine: &Engine) -> Vec<(TxId, TrackedDeposit)> {
        let mut v: Vec<_> = engine.deposits.iter().map(|(&id, d)| (id, *d)).collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }

    #[test]
    fn deposit_and_withdrawal() {
        let engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 2,
                tx: 2,
                amount: amt("2.0"),
            },
            Deposit {
                client: 1,
                tx: 3,
                amount: amt("2.0"),
            },
            Withdrawal {
                client: 1,
                tx: 4,
                amount: amt("1.5"),
            },
        ]);
        assert_eq!(balances(&engine, 1), b("1.5", "0", "1.5", false));
        assert_eq!(balances(&engine, 2), b("2", "0", "2", false));
        assert_eq!(engine.accounts().count(), 2);
    }

    #[test]
    fn withdrawal_of_everything() {
        let engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("3.1415"),
            },
            Withdrawal {
                client: 1,
                tx: 2,
                amount: amt("3.1415"),
            },
        ]);
        assert_eq!(balances(&engine, 1), b("0", "0", "0", false));
    }

    #[test]
    fn withdrawal_with_insufficient_funds() {
        let mut engine = engine_with(&[Deposit {
            client: 2,
            tx: 2,
            amount: amt("2.0"),
        }]);
        let tx = Withdrawal {
            client: 2,
            tx: 5,
            amount: amt("3.0"),
        };
        assert_rejected(&mut engine, tx, TxError::InsufficientFunds);
        assert_eq!(balances(&engine, 2), b("2", "0", "2", false));
    }

    #[test]
    fn withdrawal_from_unknown_client_creates_no_account() {
        let mut engine = Engine::new();
        let tx = Withdrawal {
            client: 7,
            tx: 1,
            amount: amt("0"),
        };
        assert_rejected(&mut engine, tx, TxError::InsufficientFunds);
        assert!(engine.account(7).is_none());
    }

    #[test]
    fn duplicate_deposit_id_is_rejected() {
        let mut engine = engine_with(&[Deposit {
            client: 1,
            tx: 1,
            amount: amt("1"),
        }]);
        let tx = Deposit {
            client: 2,
            tx: 1,
            amount: amt("5"),
        };
        assert_rejected(&mut engine, tx, TxError::DuplicateTx);
        assert!(engine.account(2).is_none());
    }

    #[test]
    fn deposit_overflowing_available_is_rejected() {
        let mut engine = engine_with(&[Deposit {
            client: 1,
            tx: 1,
            amount: Amount::from_raw(i64::MAX),
        }]);
        let tx = Deposit {
            client: 1,
            tx: 2,
            amount: Amount::from_raw(1),
        };
        assert_rejected(&mut engine, tx, TxError::Overflow);
    }

    #[test]
    fn deposit_overflowing_total_is_rejected() {
        // Available alone would fit, but available + held would not.
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: Amount::from_raw(i64::MAX - 1),
            },
            Dispute { client: 1, tx: 1 },
        ]);
        let tx = Deposit {
            client: 1,
            tx: 2,
            amount: Amount::from_raw(2),
        };
        assert_rejected(&mut engine, tx, TxError::Overflow);
    }

    #[test]
    fn dispute_holds_funds() {
        let engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 1,
                tx: 2,
                amount: amt("2.5"),
            },
            Dispute { client: 1, tx: 2 },
        ]);
        assert_eq!(balances(&engine, 1), b("1", "2.5", "3.5", false));
    }

    #[test]
    fn resolve_releases_funds_and_forgets_deposit() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Dispute { client: 1, tx: 1 },
            Resolve { client: 1, tx: 1 },
        ]);
        assert_eq!(balances(&engine, 1), b("1", "0", "1", false));
        // Once resolved, the deposit cannot be disputed again.
        assert_rejected(
            &mut engine,
            Dispute { client: 1, tx: 1 },
            TxError::UnknownTx,
        );
    }

    #[test]
    fn chargeback_withdraws_held_funds_and_locks() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 1,
                tx: 2,
                amount: amt("2.0"),
            },
            Dispute { client: 1, tx: 2 },
            Chargeback { client: 1, tx: 2 },
        ]);
        assert_eq!(balances(&engine, 1), b("1", "0", "1", true));
        assert!(!engine.deposits.contains_key(&2));
        assert_rejected(
            &mut engine,
            Dispute { client: 1, tx: 2 },
            TxError::UnknownTx,
        );
    }

    #[test]
    fn locked_account_rejects_everything() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 1,
                tx: 2,
                amount: amt("2.0"),
            },
            Deposit {
                client: 1,
                tx: 3,
                amount: amt("4.0"),
            },
            Dispute { client: 1, tx: 3 },
            Dispute { client: 1, tx: 2 },
            Chargeback { client: 1, tx: 2 },
        ]);
        let locked = TxError::AccountLocked;
        assert_rejected(
            &mut engine,
            Deposit {
                client: 1,
                tx: 9,
                amount: amt("1"),
            },
            locked,
        );
        assert_rejected(
            &mut engine,
            Withdrawal {
                client: 1,
                tx: 9,
                amount: amt("1"),
            },
            locked,
        );
        assert_rejected(&mut engine, Dispute { client: 1, tx: 1 }, locked);
        assert_rejected(&mut engine, Resolve { client: 1, tx: 3 }, locked);
        assert_rejected(&mut engine, Chargeback { client: 1, tx: 3 }, locked);
        assert_eq!(balances(&engine, 1), b("1", "4", "5", true));
    }

    #[test]
    fn dispute_errors() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 2,
                tx: 2,
                amount: amt("5.0"),
            },
            Withdrawal {
                client: 2,
                tx: 3,
                amount: amt("4.0"),
            },
            Dispute { client: 1, tx: 1 },
        ]);
        // Unknown transaction.
        assert_rejected(
            &mut engine,
            Dispute { client: 1, tx: 42 },
            TxError::UnknownTx,
        );
        // Withdrawals are not tracked, hence cannot be disputed.
        assert_rejected(
            &mut engine,
            Dispute { client: 2, tx: 3 },
            TxError::UnknownTx,
        );
        // Deposit belonging to another client.
        assert_rejected(
            &mut engine,
            Dispute { client: 2, tx: 1 },
            TxError::UnknownTx,
        );
        // Disputing twice.
        assert_rejected(
            &mut engine,
            Dispute { client: 1, tx: 1 },
            TxError::AlreadyDisputed,
        );
    }

    #[test]
    fn dispute_of_withdrawn_funds_puts_client_in_debt() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("10"),
            },
            Withdrawal {
                client: 1,
                tx: 2,
                amount: amt("10"),
            },
            Dispute { client: 1, tx: 1 },
        ]);
        assert_eq!(balances(&engine, 1), b("-10", "10", "0", false));
        // A client in debt cannot withdraw anything, not even zero.
        for amount in ["0", "1"] {
            let tx = Withdrawal {
                client: 1,
                tx: 3,
                amount: amt(amount),
            };
            assert_rejected(&mut engine, tx, TxError::InsufficientFunds);
        }
        // Deposits still go through, and lower the debt.
        let deposit = Deposit {
            client: 1,
            tx: 4,
            amount: amt("4"),
        };
        engine.apply(deposit).unwrap();
        assert_eq!(balances(&engine, 1), b("-6", "10", "4", false));
        let tx = Withdrawal {
            client: 1,
            tx: 5,
            amount: amt("0"),
        };
        assert_rejected(&mut engine, tx, TxError::InsufficientFunds);
    }

    #[test]
    fn debt_is_cleared_by_resolve_or_kept_by_chargeback() {
        let in_debt = [
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("10"),
            },
            Withdrawal {
                client: 1,
                tx: 2,
                amount: amt("7.5"),
            },
            Dispute { client: 1, tx: 1 },
        ];
        let mut engine = engine_with(&in_debt);
        engine.apply(Resolve { client: 1, tx: 1 }).unwrap();
        assert_eq!(balances(&engine, 1), b("2.5", "0", "2.5", false));
        let mut engine = engine_with(&in_debt);
        engine.apply(Chargeback { client: 1, tx: 1 }).unwrap();
        assert_eq!(balances(&engine, 1), b("-7.5", "0", "-7.5", true));
    }

    #[test]
    fn dispute_overflowing_available_is_rejected() {
        // Two deposits of the maximum, both withdrawn: disputing the second
        // would take the available funds below the minimum.
        let max = Amount::from_raw(i64::MAX);
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: max,
            },
            Withdrawal {
                client: 1,
                tx: 2,
                amount: max,
            },
            Deposit {
                client: 1,
                tx: 3,
                amount: max,
            },
            Withdrawal {
                client: 1,
                tx: 4,
                amount: max,
            },
            Dispute { client: 1, tx: 1 },
        ]);
        assert_rejected(&mut engine, Dispute { client: 1, tx: 3 }, TxError::Overflow);
    }

    #[test]
    fn dispute_overflowing_held_is_rejected() {
        // The maximum is held: disputing one more unit would overflow `held`
        // even though `available` would fit.
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: Amount::from_raw(i64::MAX),
            },
            Withdrawal {
                client: 1,
                tx: 2,
                amount: Amount::from_raw(1),
            },
            Deposit {
                client: 1,
                tx: 3,
                amount: Amount::from_raw(1),
            },
            Dispute { client: 1, tx: 1 },
        ]);
        assert_rejected(&mut engine, Dispute { client: 1, tx: 3 }, TxError::Overflow);
    }

    #[test]
    fn resolve_and_chargeback_errors() {
        let mut engine = engine_with(&[
            Deposit {
                client: 1,
                tx: 1,
                amount: amt("1.0"),
            },
            Deposit {
                client: 1,
                tx: 2,
                amount: amt("2.0"),
            },
            Dispute { client: 1, tx: 2 },
        ]);
        let settles: [fn(ClientId, TxId) -> Transaction; 2] = [
            |client, tx| Resolve { client, tx },
            |client, tx| Chargeback { client, tx },
        ];
        for settle in settles {
            assert_rejected(&mut engine, settle(1, 42), TxError::UnknownTx);
            assert_rejected(&mut engine, settle(2, 2), TxError::UnknownTx);
            assert_rejected(&mut engine, settle(1, 1), TxError::NotDisputed);
        }
    }

    #[test]
    fn error_messages() {
        let all = [
            (TxError::AccountLocked, "account is locked"),
            (TxError::DuplicateTx, "duplicate transaction id"),
            (TxError::InsufficientFunds, "insufficient available funds"),
            (TxError::Overflow, "balance overflow"),
            (TxError::UnknownTx, "unknown transaction"),
            (TxError::AlreadyDisputed, "transaction already disputed"),
            (TxError::NotDisputed, "transaction not under dispute"),
        ];
        for (err, msg) in all {
            assert_eq!(err.to_string(), msg);
        }
    }
}
