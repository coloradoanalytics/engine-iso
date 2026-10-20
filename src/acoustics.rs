//! Independently implemented acoustic terms for ISO 9613-2:2024.
//!
//! Geometry eligibility, path construction, and source integration belong to the
//! caller. These functions do not establish whole-scene conformity. Sources and
//! the limited independent evidence are cited beside each term and test.

use crate::types::{Bands, Diagnostic, Sum};

pub const FREQUENCIES: Bands = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
/// Conventional octave A-weighting corrections, applied once to received energy.
pub const A_WEIGHTING: Bands = [-26.2, -16.1, -8.6, -3.2, 0.0, 1.2, 1.0, -1.1];
/// Conventional octave C-weighting corrections.
pub const C_WEIGHTING: Bands = [-0.8, -0.2, 0.0, 0.0, 0.0, -0.2, -0.8, -3.0];
const DB_PER_NEPER_ENERGY: f64 = 10.0 / std::f64::consts::LN_10;

#[derive(Clone, Copy, Debug)]
pub struct GroundTerms {
    pub attenuation: Bands,
    pub reflection_gain: f64,
}

fn finite(value: f64, what: &str) -> Result<f64, Diagnostic> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Diagnostic::numerical(format!(
            "{what} is outside finite numerical range"
        )))
    }
}

fn nonnegative(value: f64, what: &str) -> Result<(), Diagnostic> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(Diagnostic::invalid(format!(
            "{what} must be finite and nonnegative"
        )))
    }
}

fn positive(value: f64, what: &str) -> Result<(), Diagnostic> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(Diagnostic::invalid(format!(
            "{what} must be finite and positive"
        )))
    }
}

/// Exact base-10 octave centres (1993 equation 6; 2024 clause 7.2).
pub fn exact_frequencies() -> Bands {
    std::array::from_fn(|b| 1000.0 * 10.0_f64.powf(0.3 * (b as f64 - 4.0)))
}

/// ISO 9613-1:1993 equations (3)-(6), (B.1)-(B.3), in dB/metre.
/// Relative humidity is a percentage (70, not 0.70), including below 0 °C.
pub fn atmospheric_absorption(
    temperature_c: f64,
    relative_humidity_percent: f64,
    pressure_kpa: f64,
) -> Result<Bands, Diagnostic> {
    if !temperature_c.is_finite() || !(-20.0..=50.0).contains(&temperature_c) {
        return Err(Diagnostic::invalid(
            "Atmospheric temperature must be between -20 and 50 degrees Celsius",
        ));
    }
    if !relative_humidity_percent.is_finite()
        || !(10.0..=100.0).contains(&relative_humidity_percent)
    {
        return Err(Diagnostic::invalid(
            "Relative humidity must be between 10 and 100 percent",
        ));
    }
    positive(pressure_kpa, "Atmospheric pressure")?;
    if pressure_kpa > 200.0 {
        return Err(Diagnostic::invalid(
            "Atmospheric pressure must not exceed 200 kPa",
        ));
    }
    let temperature = temperature_c + 273.15;
    let tr = temperature / 293.15;
    let pr = pressure_kpa / 101.325;
    let vapour_ratio = 10.0_f64.powf(-6.8346 * (273.16 / temperature).powf(1.261) + 4.6151);
    let h = relative_humidity_percent * vapour_ratio / pr;
    let oxygen = pr * (24.0 + 40400.0 * h * (0.02 + h) / (0.391 + h));
    let nitrogen =
        pr * tr.powf(-0.5) * (9.0 + 280.0 * h * (-4.170 * (tr.powf(-1.0 / 3.0) - 1.0)).exp());
    let classical = 1.84e-11 * tr.sqrt() / pr;
    let mut result = [0.0; 8];
    for (b, frequency) in exact_frequencies().iter().enumerate() {
        let f2 = frequency * frequency;
        let relaxation = tr.powf(-2.5)
            * (0.01275 * (-2239.1 / temperature).exp() / (oxygen + f2 / oxygen)
                + 0.1068 * (-3352.0 / temperature).exp() / (nitrogen + f2 / nitrogen));
        result[b] = finite(
            8.686 * f2 * (classical + relaxation),
            "Atmospheric absorption",
        )?;
        if result[b] <= 0.0 {
            return Err(Diagnostic::numerical(
                "Positive atmospheric absorption underflowed",
            ));
        }
    }
    Ok(result)
}

/// ISO 9613-2:2024 equation (8); full source/image reference distance in metres.
pub fn divergence(distance: f64) -> Result<f64, Diagnostic> {
    positive(distance, "Geometrical spreading distance")?;
    finite(20.0 * distance.log10() + 11.0, "Geometrical spreading")
}

/// Equation (13). Endpoint heights are physical heights above ground.
pub fn geometry_factor(hs: f64, hr: f64, dp: f64) -> Result<f64, Diagnostic> {
    nonnegative(hs, "Source height")?;
    nonnegative(hr, "Receiver height")?;
    nonnegative(dp, "Horizontal reference length")?;
    // Scaling avoids overflow for otherwise finite coordinates.
    let scale = hs.max(hr).max(dp);
    if scale == 0.0 {
        return Err(Diagnostic::unsupported(
            "Coincident ground-level endpoints have undefined ground geometry",
        ));
    }
    let (s, r, p) = (hs / scale, hr / scale, dp / scale);
    finite(
        (p * p + (s - r).powi(2)) / (p * p + (s + r).powi(2)),
        "Ground geometry factor",
    )
}

/// General method, Table 3 and equations (11)-(13), using horizontally
/// length-weighted G for the source, middle and receiver reference regions.
pub fn general_ground(
    hs: f64,
    hr: f64,
    dp: f64,
    gs: f64,
    gm: f64,
    gr: f64,
) -> Result<Bands, Diagnostic> {
    let k = geometry_factor(hs, hr, dp)?;
    for factor in [gs, gm, gr] {
        if !factor.is_finite() || !(0.0..=1.0).contains(&factor) {
            return Err(Diagnostic::invalid("Ground factors must be in [0,1]"));
        }
    }
    let distance_factor = -(-dp / 50.0).exp_m1();
    let long_distance_factor = -(-2.8e-6 * dp * dp).exp_m1();
    let endpoint = |h: f64, g: f64| -> Bands {
        let h2 = h * h;
        [
            -1.5,
            -1.5 + g
                * (1.5
                    + 3.0 * (-0.12 * (h - 5.0).powi(2)).exp() * distance_factor
                    + 5.7 * (-0.09 * h2).exp() * long_distance_factor),
            -1.5 + g * (1.5 + 8.6 * (-0.09 * h2).exp() * distance_factor),
            -1.5 + g * (1.5 + 14.0 * (-0.46 * h2).exp() * distance_factor),
            -1.5 + g * (1.5 + 5.0 * (-0.9 * h2).exp() * distance_factor),
            -1.5 * (1.0 - g),
            -1.5 * (1.0 - g),
            -1.5 * (1.0 - g),
        ]
    };
    let q = if dp <= 30.0 * (hs + hr) {
        0.0
    } else {
        1.0 - 30.0 * (hs + hr) / dp
    };
    let source = endpoint(hs, gs);
    let receiver = endpoint(hr, gr);
    let mut out = [0.0; 8];
    for b in 0..8 {
        let middle = -3.0 * q * if b == 0 { 1.0 } else { 1.0 - gm };
        let uncorrected = source[b] + receiver[b] + middle;
        // This is an energy-domain correction; multiplication of dB by k is wrong.
        out[b] = finite(
            -DB_PER_NEPER_ENERGY * (k * (-uncorrected / DB_PER_NEPER_ENERGY).exp_m1()).ln_1p(),
            "Ground attenuation",
        )?;
    }
    Ok(out)
}

/// Simplified method, equations (14)-(15). `hm` may be signed for the engine's
/// hypothetical reference convention. Geometry/profile applicability is external.
pub fn mean_height_ground(
    hm: f64,
    distance: f64,
    dp: f64,
    hs: f64,
    hr: f64,
) -> Result<GroundTerms, Diagnostic> {
    finite(hm, "Mean reference height")?;
    positive(distance, "Simplified ground reference distance")?;
    let k = geometry_factor(hs, hr, dp)?;
    let attenuation = finite(
        (4.8 - (2.0 * hm / distance) * (17.0 + 300.0 / distance)).max(0.0),
        "Simplified ground attenuation",
    )?;
    Ok(GroundTerms {
        attenuation: [attenuation; 8],
        reflection_gain: DB_PER_NEPER_ENERGY * k.ln_1p(),
    })
}

/// Equations (18)-(21), using signed path difference and nominal wavelengths.
/// The caller supplies distances from the full controlling chain, not individual
/// obstacle losses. A positive edge span selects multiple-edge diffraction.
/// Vertical losses are capped per calculation; lateral losses are never capped.
pub fn diffraction(
    delta: f64,
    d_source_edge: f64,
    edge_span: f64,
    d_edge_receiver: f64,
    reference_distance: f64,
    lateral: bool,
) -> Result<Bands, Diagnostic> {
    finite(delta, "Signed diffraction path difference")?;
    nonnegative(d_source_edge, "Source-to-first-edge length")?;
    nonnegative(edge_span, "First-to-last-edge chain length")?;
    nonnegative(d_edge_receiver, "Last-edge-to-receiver length")?;
    positive(reference_distance, "Diffraction reference distance")?;
    if lateral && delta < 0.0 {
        return Err(Diagnostic::invalid(
            "A lateral diffraction path cannot have negative path difference",
        ));
    }
    let mut result = [0.0; 8];
    for b in 0..8 {
        let wavelength = 340.0 / FREQUENCIES[b];
        let c3 = if edge_span == 0.0 {
            1.0
        } else {
            // Algebraically equation (20), without (5 lambda/e)^2 overflow.
            let e = edge_span / edge_span.max(5.0 * wavelength);
            let l = 5.0 * wavelength / edge_span.max(5.0 * wavelength);
            (e * e + l * l) / (e * e / 3.0 + l * l)
        };
        let zmin = -2.0 * wavelength / (20.0 * c3);
        if delta <= zmin {
            continue;
        }
        let kmet = if lateral {
            1.0
        } else {
            let radicand = (d_source_edge.max(d_edge_receiver) + edge_span)
                * d_source_edge.min(d_edge_receiver)
                * reference_distance
                / (2.0 * (delta - zmin));
            // Near zmin the exact limit is zero, including radicand overflow.
            (-radicand.sqrt() / 2000.0).exp()
        };
        let increment = (20.0 * c3 / wavelength) * (delta - zmin) * kmet;
        let loss = finite(
            DB_PER_NEPER_ENERGY * increment.ln_1p(),
            "Diffraction attenuation",
        )?;
        result[b] = if lateral {
            loss
        } else {
            loss.min(if edge_span > 0.0 { 25.0 } else { 20.0 })
        };
    }
    Ok(result)
}

/// Engine completed-route coupling: positive ground is removed from total V
/// once; lateral H is unchanged. V may already sum capped section losses.
pub fn screening_loss(vertical: Bands, lateral: Bands, ground: Bands) -> Result<Bands, Diagnostic> {
    let mut result = [0.0; 8];
    for b in 0..8 {
        nonnegative(vertical[b], "Vertical diffraction loss")?;
        nonnegative(lateral[b], "Lateral diffraction loss")?;
        finite(ground[b], "Ground attenuation")?;
        result[b] = finite(
            (vertical[b] - ground[b].max(0.0)).max(0.0) + lateral[b],
            "Coupled screening loss",
        )?;
    }
    Ok(result)
}

/// Equation (26). Size is min(a*cos(beta_a), h*cos(beta_h)); both distances
/// include every preceding/following bend, as required by clause 7.5.3.
pub fn reflector_eligible(
    band: usize,
    effective_size: f64,
    source_path_length: f64,
    receiver_path_length: f64,
) -> Result<bool, Diagnostic> {
    if band >= 8 {
        return Err(Diagnostic::invalid("Octave band index must be below 8"));
    }
    nonnegative(effective_size, "Effective reflector size")?;
    positive(source_path_length, "Source-to-reflector path length")?;
    positive(receiver_path_length, "Reflector-to-receiver path length")?;
    if effective_size == 0.0 {
        return Ok(false);
    }
    // 2ab/(a+b) evaluated without a*b overflow.
    let twice_harmonic_length = 2.0 / (source_path_length.recip() + receiver_path_length.recip());
    Ok(effective_size > ((340.0 / FREQUENCIES[band]) * twice_harmonic_length).sqrt())
}

/// Equations (31)-(32). Call once per physical-source/receiver/reflection family.
pub fn meteorological(hs: f64, hr: f64, dp: f64, c0: f64) -> Result<f64, Diagnostic> {
    nonnegative(hs, "Meteorological source height")?;
    nonnegative(hr, "Meteorological receiver height")?;
    nonnegative(dp, "Meteorological horizontal distance")?;
    nonnegative(c0, "Meteorological C0")?;
    let transition = 10.0 * (hs + hr);
    if dp <= transition {
        Ok(0.0)
    } else {
        finite(c0 * (1.0 - transition / dp), "Meteorological correction")
    }
}

/// Numeric zero dB is one energy unit. Silence must bypass this conversion.
pub fn energy_from_db(level: f64) -> Result<f64, Diagnostic> {
    finite(level, "Sound level")?;
    let energy = 10.0_f64.powf(level / 10.0);
    if energy.is_finite() && energy > 0.0 {
        Ok(energy)
    } else {
        Err(Diagnostic::numerical(
            "Positive sound energy overflowed or underflowed",
        ))
    }
}

pub fn level_from_energy(energy: f64) -> Result<Option<f64>, Diagnostic> {
    nonnegative(energy, "Sound energy")?;
    if energy == 0.0 {
        Ok(None)
    } else {
        Ok(Some(finite(10.0 * energy.log10(), "Sound level")?))
    }
}

pub fn a_weighted_energy(energy: Bands) -> Result<f64, Diagnostic> {
    let mut sum = Sum::default();
    for b in 0..8 {
        nonnegative(energy[b], "Octave sound energy")?;
        let weighted = energy[b] * 10.0_f64.powf(A_WEIGHTING[b] / 10.0);
        if energy[b] > 0.0 && weighted == 0.0 {
            return Err(Diagnostic::numerical(
                "Positive A-weighted sound energy underflowed",
            ));
        }
        sum.add(weighted);
    }
    finite(sum.total(), "A-weighted sound energy")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-10,
            "actual {actual}, expected {expected}"
        );
    }

    #[test]
    fn atmospheric_published_table_1g_and_1h() {
        // Visually transcribed from ISO9613-1:1993 Table1(g)/(h), printed p8.
        // Unit dB/km. Engineering tolerance covers three-digit rounding and
        // Annex B approximate saturation pressure; it is not an ISO/TR interval.
        let cases = [
            (
                10.0,
                10.0,
                [0.342, 0.788, 2.29, 7.52, 21.6, 42.3, 57.3, 69.4],
            ),
            (
                10.0,
                70.0,
                [0.122, 0.411, 1.04, 1.93, 3.66, 9.66, 32.8, 117.0],
            ),
            (
                10.0,
                100.0,
                [0.0885, 0.322, 0.963, 2.03, 3.55, 7.71, 23.5, 83.7],
            ),
            (
                15.0,
                70.0,
                [0.105, 0.381, 1.13, 2.36, 4.08, 8.75, 26.4, 93.7],
            ),
        ];
        for (temperature, humidity, expected) in cases {
            let actual = atmospheric_absorption(temperature, humidity, 101.325).unwrap();
            for b in 0..8 {
                assert!(
                    (actual[b] * 1000.0 / expected[b] - 1.0).abs() < 0.007,
                    "{temperature} C {humidity}% band {b}: {} versus {}",
                    actual[b] * 1000.0,
                    expected[b]
                );
            }
        }
    }

    #[test]
    fn divergence_and_meteorology_have_analytic_limits() {
        close(divergence(1.0).unwrap(), 11.0);
        close(divergence(100.0).unwrap(), 51.0);
        close(meteorological(2.0, 3.0, 50.0, 4.0).unwrap(), 0.0);
        close(meteorological(2.0, 3.0, 100.0, 4.0).unwrap(), 2.0);
        assert!(divergence(0.0).is_err());
    }

    #[test]
    fn hard_ground_and_geometric_correction_are_energy_based() {
        // hs=hr=5,dp=10 => Kgeo=1/2, q=0, Agr'=-3 in all bands.
        let expected = -10.0 * ((1.0 + 10.0_f64.powf(0.3)) / 2.0).log10();
        for actual in general_ground(5.0, 5.0, 10.0, 0.0, 0.0, 0.0).unwrap() {
            close(actual, expected);
        }
        // Equal heights with zero horizontal distance have Kgeo=0.
        for actual in general_ground(5.0, 5.0, 0.0, 0.0, 0.0, 0.0).unwrap() {
            close(actual, 0.0);
        }
        // Ground endpoints: q=1, Kgeo=1, hard ground is exactly -6 dB.
        for actual in general_ground(0.0, 0.0, 100.0, 0.0, 0.0, 0.0).unwrap() {
            close(actual, -6.0);
        }
    }

    #[test]
    fn simplified_ground_gain_is_separate_and_shared() {
        let terms = mean_height_ground(2.0, 100.0, 100.0, 0.0, 0.0).unwrap();
        assert_eq!(terms.attenuation, [4.0; 8]);
        close(terms.reflection_gain, 10.0 * 2.0_f64.log10());
        assert_eq!(
            mean_height_ground(100.0, 100.0, 100.0, 0.0, 0.0)
                .unwrap()
                .attenuation,
            [0.0; 8]
        );
        assert!(
            mean_height_ground(-1.0, 100.0, 100.0, 0.0, 0.0)
                .unwrap()
                .attenuation[0]
                > 4.8
        );
    }

    #[test]
    fn signed_diffraction_and_vertical_caps() {
        let zmin = -0.1 * (340.0 / 1000.0);
        close(
            diffraction(zmin, 10.0, 0.0, 10.0, 20.0, false).unwrap()[4],
            0.0,
        );
        // At z=0, lateral Kmet=1: Dz=10log10(3), every band.
        for actual in diffraction(0.0, 10.0, 0.0, 10.0, 20.0, true).unwrap() {
            close(actual, 10.0 * 3.0_f64.log10());
        }
        assert_eq!(
            diffraction(1e6, 10.0, 0.0, 10.0, 20.0, false).unwrap(),
            [20.0; 8]
        );
        assert_eq!(
            diffraction(1e6, 10.0, 5.0, 10.0, 20.0, false).unwrap(),
            [25.0; 8]
        );
        assert!(diffraction(1e6, 10.0, 5.0, 10.0, 20.0, true).unwrap()[0] > 25.0);
        // Independent reduction of eq21: dSS=dSR=10,d=20,z=0,e=0,
        // lambda=.34 => exponent=-sqrt(2000/.068)/2000.
        let expected = 10.0 * (1.0 + 2.0 * (-(2000.0_f64 / 0.068).sqrt() / 2000.0).exp()).log10();
        close(
            diffraction(0.0, 10.0, 0.0, 10.0, 20.0, false).unwrap()[4],
            expected,
        );
    }

    #[test]
    fn completed_section_losses_are_not_recapped() {
        assert_eq!(
            screening_loss([40.0; 8], [31.0; 8], [4.0; 8]).unwrap(),
            [67.0; 8]
        );
        assert_eq!(
            screening_loss([4.0; 8], [31.0; 8], [-3.0; 8]).unwrap(),
            [35.0; 8]
        );
        assert_eq!(
            screening_loss([2.0; 8], [31.0; 8], [4.0; 8]).unwrap(),
            [31.0; 8]
        );
    }

    #[test]
    fn reflector_strict_size_and_energy_range() {
        // lambda=.34,a=b=10 => critical size sqrt(3.4).
        let critical = 3.4_f64.sqrt();
        assert!(!reflector_eligible(4, critical, 10.0, 10.0).unwrap());
        assert!(reflector_eligible(4, critical * 1.001, 10.0, 10.0).unwrap());
        close(energy_from_db(0.0).unwrap(), 1.0);
        assert_eq!(level_from_energy(0.0).unwrap(), None);
        assert!(energy_from_db(-10000.0).is_err());
        assert!(energy_from_db(10000.0).is_err());
        let mut one_band = [0.0; 8];
        one_band[4] = 1.0;
        close(a_weighted_energy(one_band).unwrap(), 1.0);
    }
}
