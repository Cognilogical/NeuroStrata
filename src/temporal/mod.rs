//! Temporal bitemporal queries (in-house): valid-time + transaction-time intervals.
#[derive(Debug, Clone)]
pub struct Bitemporal { pub valid_from: i64, pub valid_to: i64, pub tx_from: i64, pub tx_to: i64 }
impl Bitemporal {
    pub fn contains_valid(&self, t: i64) -> bool { t >= self.valid_from && t < self.valid_to }
    pub fn contains_tx(&self, t: i64) -> bool { t >= self.tx_from && t < self.tx_to }
}
#[cfg(test)] mod tests { use super::*; #[test] fn basic() { let b=Bitemporal{valid_from:0,valid_to:10,tx_from:0,tx_to:10}; assert!(b.contains_valid(5)); } }
