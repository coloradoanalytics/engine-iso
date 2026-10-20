use serde::{Deserialize, Serialize};
pub type Point = [f64; 3];
pub type Bands = [f64; 8];
/// Serde for band arrays as IEEE-754 bit patterns: exact for every value,
/// non-finite ones included, which JSON numbers cannot carry.
pub mod band_bits {
    use super::Bands;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(bands: &Bands, s: S) -> Result<S::Ok, S::Error> {
        bands.map(f64::to_bits).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bands, D::Error> {
        Ok(<[u64; 8]>::deserialize(d)?.map(f64::from_bits))
    }
}
/// One value as its IEEE-754 bit pattern.
pub mod f64_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(value.to_bits())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(f64::from_bits(u64::deserialize(d)?))
    }
}
/// `band_bits` for optional per-band values such as cutoffs.
pub mod optional_band_bits {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    type Optional = [Option<f64>; 8];
    pub fn serialize<S: Serializer>(bands: &Optional, s: S) -> Result<S::Ok, S::Error> {
        bands.map(|b| b.map(f64::to_bits)).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Optional, D::Error> {
        Ok(<[Option<u64>; 8]>::deserialize(d)?.map(|b| b.map(f64::from_bits)))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}
impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            field: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }
    pub fn numerical(message: impl Into<String>) -> Self {
        Self::new("nonfinite_result", message)
    }
    pub fn cancelled() -> Self {
        Self::new("cancelled", "Calculation cancelled")
    }
    pub fn at(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Diagnostic {}
pub fn check_cancel(cancel: &std::sync::atomic::AtomicBool) -> Result<(), Diagnostic> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(Diagnostic::cancelled())
    } else {
        Ok(())
    }
}
/// Neumaier accumulation: reduction order is fixed by physical input identity.
#[derive(Clone, Copy, Default, Debug)]
pub struct Sum {
    value: f64,
    correction: f64,
}
impl Sum {
    pub fn add(&mut self, x: f64) {
        let next = self.value + x;
        self.correction += if self.value.abs() >= x.abs() {
            (self.value - next) + x
        } else {
            (x - next) + self.value
        };
        self.value = next;
    }
    pub fn total(self) -> f64 {
        self.value + self.correction
    }
}
/// Exact accumulation (Shewchuk non-overlapping partials, as in Python's
/// `math.fsum`). The partials represent the sum without rounding, so `total`
/// is the correctly rounded sum of every value added: it does not depend on
/// addition order, on how work was split, or on merge order. A non-finite
/// value or an overflowing partial makes the total non-finite.
#[derive(Clone, Default, Debug)]
pub struct Exact {
    partials: smallvec::SmallVec<[f64; 2]>,
    special: f64,
}
/// Carried as the bit patterns of its partials and special value, so a sum
/// merged on another machine stays exact.
impl Serialize for Exact {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let partials: Vec<u64> = self.partials.iter().map(|p| p.to_bits()).collect();
        (partials, self.special.to_bits()).serialize(s)
    }
}
impl<'de> Deserialize<'de> for Exact {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (partials, special) = <(Vec<u64>, u64)>::deserialize(d)?;
        // Rebuilding through `add` restores the partials' invariants.
        let mut sum = Exact::default();
        partials
            .into_iter()
            .for_each(|p| sum.add(f64::from_bits(p)));
        sum.special = f64::from_bits(special);
        Ok(sum)
    }
}
impl Exact {
    pub fn add(&mut self, value: f64) {
        if !value.is_finite() {
            self.special += value;
            return;
        }
        let mut x = value;
        let mut kept = 0;
        for j in 0..self.partials.len() {
            let mut y = self.partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            if !hi.is_finite() {
                self.special += hi;
                return;
            }
            let lo = y - (hi - x);
            if lo != 0. {
                self.partials[kept] = lo;
                kept += 1;
            }
            x = hi;
        }
        self.partials.truncate(kept);
        if x != 0. {
            self.partials.push(x);
        }
    }
    pub fn merge(&mut self, other: &Exact) {
        for p in &other.partials {
            self.add(*p);
        }
        self.special += other.special;
    }
    pub fn total(&self) -> f64 {
        if self.special != 0. || self.special.is_nan() {
            return self.special;
        }
        let p = &self.partials;
        let mut n = p.len();
        if n == 0 {
            return 0.;
        }
        n -= 1;
        let mut hi = p[n];
        let mut lo = 0.;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = p[n];
            hi = x + y;
            lo = y - (hi - x);
            if lo != 0. {
                break;
            }
        }
        // Round half-even correctly when the remaining partials push the
        // exact sum past the halfway point of `lo`.
        if n > 0 && ((lo < 0. && p[n - 1] < 0.) || (lo > 0. && p[n - 1] > 0.)) {
            let y = lo * 2.;
            let x = hi + y;
            if y == x - hi {
                hi = x;
            }
        }
        hi
    }
}
pub fn positive(value: f64, what: &str) -> Result<f64, Diagnostic> {
    if !value.is_finite() || value <= 0. {
        Err(Diagnostic::numerical(format!(
            "{what} overflow, underflow or precision exhaustion"
        )))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod exact_tests {
    use super::Exact;

    fn total(values: &[f64]) -> f64 {
        let mut sum = Exact::default();
        values.iter().for_each(|v| sum.add(*v));
        sum.total()
    }
    /// Deterministic xorshift values spanning many magnitudes.
    fn values(n: usize, seed: u64) -> Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let mantissa = (state >> 11) as f64 / (1u64 << 53) as f64;
                mantissa * 10f64.powi((state % 40) as i32 - 30)
            })
            .collect()
    }

    #[test]
    fn known_sums_are_correctly_rounded() {
        assert_eq!(total(&[0.1; 10]), 1.0);
        assert_eq!(total(&[1e100, 1., -1e100]), 1.);
        assert_eq!(total(&[1., 1e-16, 1e-16]), 1.0000000000000002);
        assert_eq!(total(&[]), 0.);
        assert_eq!(total(&[1., f64::INFINITY]), f64::INFINITY);
        assert_eq!(total(&[f64::MAX, f64::MAX]), f64::INFINITY);
    }

    #[test]
    fn totals_do_not_depend_on_order_or_partition() {
        let v = values(5000, 0x9e37_79b9_7f4a_7c15);
        let reference = total(&v);
        let mut reversed = v.clone();
        reversed.reverse();
        assert_eq!(total(&reversed).to_bits(), reference.to_bits());
        let mut interleaved: Vec<f64> = v.iter().step_by(2).copied().collect();
        interleaved.extend(v.iter().skip(1).step_by(2));
        assert_eq!(total(&interleaved).to_bits(), reference.to_bits());
        for parts in [2, 3, 7, 64, 5000] {
            let mut merged = Exact::default();
            for chunk in v.chunks(v.len().div_ceil(parts)).rev() {
                let mut part = Exact::default();
                chunk.iter().for_each(|x| part.add(*x));
                merged.merge(&part);
            }
            assert_eq!(merged.total().to_bits(), reference.to_bits(), "{parts}");
        }
    }
}
