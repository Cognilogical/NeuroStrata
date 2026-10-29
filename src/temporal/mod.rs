//! Temporal validity helpers (in-house).
#[derive(Debug, Clone, Copy)]
pub struct Interval { pub valid_from: i64, pub valid_to: i64, pub tx_from: i64, pub tx_to: i64 }
impl Interval {
    pub fn contains_valid(&self, t: i64) -> bool { t >= self.valid_from && t < self.valid_to }
    pub fn contains_tx(&self, t: i64) -> bool { t >= self.tx_from && t < self.tx_to }
    pub fn is_current(&self, now: i64) -> bool { self.contains_valid(now) && self.contains_tx(now) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid() {
        let i = Interval { valid_from: 10, valid_to: 20, tx_from: 0, tx_to: i64::MAX };
        assert!(i.contains_valid(15));
        assert!(!i.contains_valid(25));
        assert!(i.is_current(15));
    }
}
