//! Sigmoid error rate model for predictive frequency optimization.
//!
//! Models the relationship between frequency and error rate as a sigmoid curve:
//!   error_rate(f) = 1 / (1 + exp(-k * (f - f_cliff)))
//!
//! With 3+ observations of (frequency, error_rate), we can fit this model
//! and predict the cliff frequency without actually testing it — potentially
//! saving 1-2 binary search iterations.
//!
//! This is a genuine algorithmic innovation: no competing firmware (VNish,
//! LuxOS, BraiinsOS+) models the error curve. They all use brute-force search.

use serde::{Deserialize, Serialize};

/// Sigmoid error rate model: error_rate(f) = 1 / (1 + exp(-k * (f - f_cliff)))
///
/// Fitted via logit-linear regression on observed (frequency, error_rate) pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigmoidModel {
    /// Predicted cliff frequency (MHz) — the inflection point where
    /// error rate transitions from low to high.
    pub f_cliff_mhz: f64,
    /// Steepness parameter — how sharp the error cliff is.
    /// Higher k = sharper transition. Typical range: 0.01-0.5.
    pub steepness_k: f64,
    /// Coefficient of determination (R²) — fit quality (0-1).
    /// Above 0.85 is considered usable for prediction.
    pub r_squared: f64,
}

/// Minimum observations needed to attempt a fit.
const MIN_OBSERVATIONS: usize = 3;

/// Minimum R² for a fit to be considered usable.
const MIN_R_SQUARED: f64 = 0.85;

/// Small epsilon to avoid log(0) or log(inf) in logit transform.
/// Minimum error rate for logit transform. Using 1e-4 (0.01%) instead of 1e-6
/// to prevent extreme logit values from dominating the linear regression.
/// Observations with error rate below this are "too clean" to inform the sigmoid.
const EPSILON: f64 = 1e-4;

impl SigmoidModel {
    /// Fit a sigmoid model to observed (frequency_mhz, error_rate_fraction) pairs.
    ///
    /// Uses logit-linear regression:
    ///   logit(p) = ln(p/(1-p)) = k * f - k * f_cliff
    ///
    /// Linear regression on (f, logit(error_rate)) gives:
    ///   slope = k, intercept = -k * f_cliff → f_cliff = -intercept/slope
    ///
    /// Returns None if:
    ///   - Fewer than 3 observations
    ///   - All error rates are 0 or all are 1 (no variation)
    ///   - R² < 0.85 (poor fit)
    ///   - Negative steepness (error decreases with frequency, physically nonsensical)
    pub fn fit(observations: &[(u16, f64)]) -> Option<Self> {
        if observations.len() < MIN_OBSERVATIONS {
            return None;
        }

        // Transform to logit space. All observations are kept — clean data (0% errors)
        // is critical for anchoring the left side of the sigmoid. The EPSILON clamp
        // prevents log(0) while preserving the data point's frequency information.
        // Only skip observations at exactly 100% error (chip completely dead at that
        // frequency — this is noise, not signal about the cliff location).
        let mut logit_pairs: Vec<(f64, f64)> = Vec::new();
        for &(freq, err_rate) in observations {
            if err_rate >= 0.999 {
                continue; // Completely dead at this frequency — not informative
            }
            let p = err_rate.clamp(EPSILON, 1.0 - EPSILON);
            let logit = (p / (1.0 - p)).ln();
            logit_pairs.push((freq as f64, logit));
        }

        if logit_pairs.len() < MIN_OBSERVATIONS {
            return None;
        }

        // Linear regression: logit = slope * freq + intercept
        let n = logit_pairs.len() as f64;
        let sum_x: f64 = logit_pairs.iter().map(|(x, _)| x).sum();
        let sum_y: f64 = logit_pairs.iter().map(|(_, y)| y).sum();
        let sum_xy: f64 = logit_pairs.iter().map(|(x, y)| x * y).sum();
        let sum_x2: f64 = logit_pairs.iter().map(|(x, _)| x * x).sum();

        let denom = n * sum_x2 - sum_x * sum_x;
        if denom.abs() < 1e-10 {
            return None; // All same frequency — can't fit
        }

        let slope = (n * sum_xy - sum_x * sum_y) / denom;
        let intercept = (sum_y - slope * sum_x) / n;

        // slope = k, intercept = -k * f_cliff → f_cliff = -intercept / slope
        if slope <= 0.0 {
            return None; // Error rate must increase with frequency
        }

        let k = slope;
        let f_cliff = -intercept / slope;

        // Compute R²
        let mean_y = sum_y / n;
        let ss_tot: f64 = logit_pairs.iter().map(|(_, y)| (y - mean_y).powi(2)).sum();
        let ss_res: f64 = logit_pairs
            .iter()
            .map(|(x, y)| {
                let predicted = slope * x + intercept;
                (y - predicted).powi(2)
            })
            .sum();

        let r_squared = if ss_tot > 1e-10 {
            1.0 - ss_res / ss_tot
        } else {
            0.0
        };

        if r_squared < MIN_R_SQUARED {
            return None;
        }

        // Sanity check: f_cliff should be in a reasonable range
        let min_freq = observations.iter().map(|&(f, _)| f).min().unwrap_or(0) as f64;
        let max_freq = observations.iter().map(|&(f, _)| f).max().unwrap_or(0) as f64;
        let range = max_freq - min_freq;
        // Allow prediction within 50% beyond observed range
        if f_cliff < min_freq - range * 0.5 || f_cliff > max_freq + range * 0.5 {
            return None;
        }

        Some(Self {
            f_cliff_mhz: f_cliff,
            steepness_k: k,
            r_squared,
        })
    }

    /// Predict error rate at a given frequency using the fitted model.
    ///
    /// Returns a fraction (0.0-1.0).
    pub fn predict_error_rate(&self, freq_mhz: u16) -> f64 {
        let x = self.steepness_k * (freq_mhz as f64 - self.f_cliff_mhz);
        1.0 / (1.0 + (-x).exp())
    }

    /// Find the frequency where error rate equals the given threshold.
    ///
    /// Inverse sigmoid: f = f_cliff + ln(threshold/(1-threshold)) / k
    pub fn frequency_at_threshold(&self, threshold_pct: f64) -> u16 {
        let p = (threshold_pct / 100.0).clamp(EPSILON, 1.0 - EPSILON);
        let logit_p = (p / (1.0 - p)).ln();
        let f = self.f_cliff_mhz + logit_p / self.steepness_k;
        f.round().max(0.0) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_fit_perfect_data() {
        // Generate perfect sigmoid data: f_cliff=600, k=0.1
        let observations: Vec<(u16, f64)> = vec![
            (500, 0.0001),
            (550, 0.007),
            (580, 0.12),
            (600, 0.50),
            (620, 0.88),
            (650, 0.993),
            (700, 0.9999),
        ];

        let model = SigmoidModel::fit(&observations);
        assert!(model.is_some(), "Should fit perfect sigmoid data");
        let m = model.unwrap();
        assert!(
            (m.f_cliff_mhz - 600.0).abs() < 10.0,
            "f_cliff should be ~600, got {:.1}",
            m.f_cliff_mhz
        );
        assert!(m.steepness_k > 0.0, "k should be positive");
        assert!(
            m.r_squared > 0.9,
            "R² should be > 0.9, got {:.3}",
            m.r_squared
        );
    }

    #[test]
    fn test_sigmoid_fit_noisy_data() {
        // Noisy data with clear trend
        let observations: Vec<(u16, f64)> = vec![
            (500, 0.001),
            (550, 0.005),
            (600, 0.02),
            (625, 0.15),
            (650, 0.45),
            (675, 0.80),
            (700, 0.95),
        ];

        let model = SigmoidModel::fit(&observations);
        assert!(model.is_some(), "Should fit noisy sigmoid data");
        let m = model.unwrap();
        assert!(m.f_cliff_mhz > 600.0 && m.f_cliff_mhz < 700.0);
    }

    #[test]
    fn test_sigmoid_fit_insufficient_points() {
        let observations: Vec<(u16, f64)> = vec![(500, 0.01), (600, 0.5)];
        assert!(SigmoidModel::fit(&observations).is_none());
    }

    #[test]
    fn test_sigmoid_cliff_prediction() {
        let model = SigmoidModel {
            f_cliff_mhz: 650.0,
            steepness_k: 0.1,
            r_squared: 0.95,
        };

        // At cliff frequency, error rate should be ~50%
        let err_at_cliff = model.predict_error_rate(650);
        assert!(
            (err_at_cliff - 0.5).abs() < 0.01,
            "Error at cliff should be ~50%, got {:.3}",
            err_at_cliff
        );

        // Well below cliff: low error
        assert!(model.predict_error_rate(550) < 0.01);
        // Well above cliff: high error
        assert!(model.predict_error_rate(750) > 0.99);
    }

    #[test]
    fn test_sigmoid_frequency_at_threshold() {
        let model = SigmoidModel {
            f_cliff_mhz: 650.0,
            steepness_k: 0.1,
            r_squared: 0.95,
        };

        // At 0.5% threshold, should be below cliff
        let freq_05 = model.frequency_at_threshold(0.5);
        assert!(
            freq_05 < 650,
            "0.5% threshold freq should be below cliff (650), got {}",
            freq_05
        );
        assert!(freq_05 > 500, "Should be reasonable, got {}", freq_05);
    }

    #[test]
    fn test_sigmoid_fit_all_zero_errors() {
        // All zero error rates — no sigmoid to fit
        let observations: Vec<(u16, f64)> = vec![(500, 0.0), (600, 0.0), (700, 0.0)];
        // With EPSILON clamping these become very small but uniform — R² will be low
        let model = SigmoidModel::fit(&observations);
        assert!(model.is_none(), "Should not fit all-zero data");
    }

    #[test]
    fn test_sigmoid_fit_negative_slope() {
        // Error rate decreasing with frequency — physically nonsensical
        let observations: Vec<(u16, f64)> = vec![(500, 0.90), (600, 0.50), (700, 0.01)];
        let model = SigmoidModel::fit(&observations);
        assert!(
            model.is_none(),
            "Should reject negative slope (error decreasing with freq)"
        );
    }
}
