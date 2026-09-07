/// Gaussian lookup table for pitch error evaluation (§9.2)
/// Step: 0.1 cents, Range: 0.0 ..= 600.0 cents (6001 entries)
/// Value: scaled integer in 0 ..= 1_000_000
pub struct GaussianLut {
    table: Vec<u32>,
}

impl GaussianLut {
    pub fn new(tolerance_cents: f64) -> Self {
        let size = 6001; // 0.0 to 600.0 in 0.1 increments
        let mut table = Vec::with_capacity(size);
        for i in 0..size {
            let cents = i as f64 * 0.1;
            let ratio = cents / tolerance_cents;
            let p = (-0.5 * ratio * ratio).exp();
            let scaled = (p * 1_000_000.0).round() as u32;
            table.push(scaled.min(1_000_000));
        }
        Self { table }
    }

    /// Look up integer p(t) in 0 ..= 1_000_000 for abs error in cents
    pub fn lookup(&self, abs_cents: f64) -> u32 {
        if abs_cents < 0.0 {
            return self.lookup(-abs_cents);
        }
        let index = (abs_cents * 10.0).round() as usize;
        if index < self.table.len() {
            self.table[index]
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lut_values() {
        let lut = GaussianLut::new(35.0);
        assert_eq!(lut.lookup(0.0), 1_000_000); // 0 error -> 1.0
                                                // At 35 cents, exp(-0.5) ≈ 0.60653066 -> 606531
        let p35 = lut.lookup(35.0);
        assert!((p35 as i32 - 606531).abs() <= 2);
        // Beyond 600 cents -> 0
        assert_eq!(lut.lookup(650.0), 0);
    }
}
