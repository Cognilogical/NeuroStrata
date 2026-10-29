//! Time-series metrics (in-house).
#[derive(Debug, Default, Clone)]
pub struct Series { pub points: Vec<(i64, f64)> }
impl Series {
    pub fn push(&mut self, t: i64, v: f64) { self.points.push((t,v)); }
    pub fn mean(&self) -> f64 {
        if self.points.is_empty() { return 0.0; }
        self.points.iter().map(|p|p.1).sum::<f64>() / self.points.len() as f64
    }
    pub fn last(&self) -> Option<f64> { self.points.last().map(|p|p.1) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mean() {
        let mut s = Series::default();
        s.push(1, 2.0); s.push(2, 4.0);
        assert_eq!(s.mean(), 3.0);
        assert_eq!(s.last(), Some(4.0));
    }
}
