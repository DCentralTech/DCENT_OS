//! Maximum clock-rate fitting for voltage/frequency characterization.
//!
//! The autotuner records measured stable frequencies at several voltage points.
//! This module fits a small quadratic model, MCR(V), that can be persisted or
//! used by higher-level DPS policy without depending on chain control code.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct McrSample {
    pub voltage_mv: u16,
    pub max_stable_mhz: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QuadraticFunc {
    /// Coefficient for voltage_v^2.
    pub a: f64,
    /// Coefficient for voltage_v.
    pub b: f64,
    /// Constant term.
    pub c: f64,
}

impl QuadraticFunc {
    pub fn predict_voltage_v(&self, voltage_v: f64) -> f64 {
        self.a * voltage_v * voltage_v + self.b * voltage_v + self.c
    }

    pub fn predict_voltage_mv(&self, voltage_mv: u16) -> f64 {
        self.predict_voltage_v(voltage_mv as f64 / 1000.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McrFit {
    pub curve: QuadraticFunc,
    pub sample_count: usize,
    pub rms_error_mhz: f64,
    pub min_voltage_mv: u16,
    pub max_voltage_mv: u16,
}

pub fn fit_mcr_curve(samples: &[McrSample]) -> Option<McrFit> {
    let samples: Vec<McrSample> = samples
        .iter()
        .copied()
        .filter(|sample| {
            sample.voltage_mv > 0
                && sample.max_stable_mhz.is_finite()
                && sample.max_stable_mhz > 0.0
        })
        .collect();

    if samples.is_empty() {
        return None;
    }

    let curve = match samples.len() {
        1 => QuadraticFunc {
            a: 0.0,
            b: 0.0,
            c: samples[0].max_stable_mhz,
        },
        2 => fit_linear(&samples),
        _ => fit_quadratic(&samples).unwrap_or_else(|| fit_linear(&samples)),
    };

    let rms_error_mhz = rms_error(&curve, &samples);
    let min_voltage_mv = samples
        .iter()
        .map(|sample| sample.voltage_mv)
        .min()
        .unwrap_or_default();
    let max_voltage_mv = samples
        .iter()
        .map(|sample| sample.voltage_mv)
        .max()
        .unwrap_or_default();

    Some(McrFit {
        curve,
        sample_count: samples.len(),
        rms_error_mhz,
        min_voltage_mv,
        max_voltage_mv,
    })
}

fn fit_linear(samples: &[McrSample]) -> QuadraticFunc {
    let n = samples.len() as f64;
    let sum_x: f64 = samples
        .iter()
        .map(|sample| sample.voltage_mv as f64 / 1000.0)
        .sum();
    let sum_y: f64 = samples.iter().map(|sample| sample.max_stable_mhz).sum();
    let sum_xx: f64 = samples
        .iter()
        .map(|sample| {
            let x = sample.voltage_mv as f64 / 1000.0;
            x * x
        })
        .sum();
    let sum_xy: f64 = samples
        .iter()
        .map(|sample| {
            let x = sample.voltage_mv as f64 / 1000.0;
            x * sample.max_stable_mhz
        })
        .sum();

    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return QuadraticFunc {
            a: 0.0,
            b: 0.0,
            c: sum_y / n,
        };
    }

    let b = (n * sum_xy - sum_x * sum_y) / denom;
    let c = (sum_y - b * sum_x) / n;
    QuadraticFunc { a: 0.0, b, c }
}

fn fit_quadratic(samples: &[McrSample]) -> Option<QuadraticFunc> {
    let mut sx = [0.0_f64; 5];
    let mut sy = [0.0_f64; 3];

    for sample in samples {
        let x = sample.voltage_mv as f64 / 1000.0;
        let y = sample.max_stable_mhz;
        let mut pow = 1.0;
        for item in &mut sx {
            *item += pow;
            pow *= x;
        }
        sy[0] += y;
        sy[1] += x * y;
        sy[2] += x * x * y;
    }

    // Normal equations for y = a*x^2 + b*x + c, ordered [a, b, c].
    let mut matrix = [
        [sx[4], sx[3], sx[2], sy[2]],
        [sx[3], sx[2], sx[1], sy[1]],
        [sx[2], sx[1], sx[0], sy[0]],
    ];

    solve_3x3(&mut matrix).map(|solution| QuadraticFunc {
        a: solution[0],
        b: solution[1],
        c: solution[2],
    })
}

fn solve_3x3(matrix: &mut [[f64; 4]; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let pivot = (col..3).max_by(|&left, &right| {
            matrix[left][col]
                .abs()
                .partial_cmp(&matrix[right][col].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;

        if matrix[pivot][col].abs() < 1e-12 {
            return None;
        }

        if pivot != col {
            matrix.swap(pivot, col);
        }

        let pivot_value = matrix[col][col];
        for item in &mut matrix[col][col..4] {
            *item /= pivot_value;
        }

        for row in 0..3 {
            if row == col {
                continue;
            }
            let factor = matrix[row][col];
            for idx in col..4 {
                matrix[row][idx] -= factor * matrix[col][idx];
            }
        }
    }

    Some([matrix[0][3], matrix[1][3], matrix[2][3]])
}

fn rms_error(curve: &QuadraticFunc, samples: &[McrSample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_sq: f64 = samples
        .iter()
        .map(|sample| {
            let predicted = curve.predict_voltage_mv(sample.voltage_mv);
            let err = predicted - sample.max_stable_mhz;
            err * err
        })
        .sum();
    (sum_sq / samples.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_quadratic_recovers_known_curve() {
        let samples: Vec<McrSample> = [8_800, 8_900, 9_000, 9_100, 9_200]
            .iter()
            .map(|&voltage_mv| {
                let v = voltage_mv as f64 / 1000.0;
                McrSample {
                    voltage_mv,
                    max_stable_mhz: 40.0 * v * v + 15.0 * v + 100.0,
                }
            })
            .collect();

        let fit = fit_mcr_curve(&samples).expect("fit");
        assert_eq!(fit.sample_count, 5);
        assert!(fit.rms_error_mhz < 0.001);
        assert!(
            (fit.curve.predict_voltage_mv(9_050) - (40.0 * 9.05 * 9.05 + 15.0 * 9.05 + 100.0))
                .abs()
                < 0.01
        );
    }

    #[test]
    fn fit_two_points_uses_linear_fallback() {
        let fit = fit_mcr_curve(&[
            McrSample {
                voltage_mv: 8_800,
                max_stable_mhz: 600.0,
            },
            McrSample {
                voltage_mv: 9_200,
                max_stable_mhz: 700.0,
            },
        ])
        .expect("fit");

        assert_eq!(fit.curve.a, 0.0);
        assert!((fit.curve.predict_voltage_mv(9_000) - 650.0).abs() < 0.001);
    }

    #[test]
    fn fit_filters_invalid_samples() {
        let fit = fit_mcr_curve(&[
            McrSample {
                voltage_mv: 0,
                max_stable_mhz: 600.0,
            },
            McrSample {
                voltage_mv: 9_000,
                max_stable_mhz: f64::NAN,
            },
            McrSample {
                voltage_mv: 9_100,
                max_stable_mhz: 650.0,
            },
        ])
        .expect("fit");

        assert_eq!(fit.sample_count, 1);
        assert_eq!(fit.curve.predict_voltage_mv(8_900), 650.0);
    }
}
