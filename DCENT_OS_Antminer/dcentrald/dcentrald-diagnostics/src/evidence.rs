//! Typed provenance for diagnostic and manufacturing evidence.
//!
//! A numeric value is not automatically a measurement. Runtime voltage
//! setpoints, zero cumulative CRC counters, model-derived EEPROM metadata and
//! absent sensor reads must remain distinguishable at every grading boundary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Measured,
    Commanded,
    Inferred,
    Unavailable,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Commanded => "commanded",
            Self::Inferred => "inferred",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceQuality {
    /// Direct observation with protocol/schema/checksum validation.
    Validated,
    /// Direct sensor or counter observation without stronger validation.
    Observed,
    /// Derived estimate or proxy.
    Estimated,
    /// No defensible quality statement is available.
    Unknown,
}

impl Default for EvidenceQuality {
    fn default() -> Self {
        Self::Unknown
    }
}

/// One value plus the provenance needed to decide whether it can support a
/// measured diagnostic verdict. Construction helpers keep kind and quality
/// coherent; legacy deserialization defaults to explicit Unavailable evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticEvidence<T> {
    kind: EvidenceKind,
    value: Option<T>,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_at_epoch_s: Option<u64>,
    quality: EvidenceQuality,
}

impl<T> Default for DiagnosticEvidence<T> {
    fn default() -> Self {
        Self::unavailable("legacy_or_missing_provenance")
    }
}

impl<T> DiagnosticEvidence<T> {
    pub fn measured(value: T, source: impl Into<String>, observed_at_epoch_s: Option<u64>) -> Self {
        Self {
            kind: EvidenceKind::Measured,
            value: Some(value),
            source: source.into(),
            observed_at_epoch_s,
            quality: EvidenceQuality::Observed,
        }
    }

    pub fn measured_validated(
        value: T,
        source: impl Into<String>,
        observed_at_epoch_s: Option<u64>,
    ) -> Self {
        Self {
            kind: EvidenceKind::Measured,
            value: Some(value),
            source: source.into(),
            observed_at_epoch_s,
            quality: EvidenceQuality::Validated,
        }
    }

    pub fn commanded(
        value: T,
        source: impl Into<String>,
        observed_at_epoch_s: Option<u64>,
    ) -> Self {
        Self {
            kind: EvidenceKind::Commanded,
            value: Some(value),
            source: source.into(),
            observed_at_epoch_s,
            quality: EvidenceQuality::Observed,
        }
    }

    pub fn inferred(value: T, source: impl Into<String>, observed_at_epoch_s: Option<u64>) -> Self {
        Self {
            kind: EvidenceKind::Inferred,
            value: Some(value),
            source: source.into(),
            observed_at_epoch_s,
            quality: EvidenceQuality::Estimated,
        }
    }

    pub fn unavailable(source: impl Into<String>) -> Self {
        Self {
            kind: EvidenceKind::Unavailable,
            value: None,
            source: source.into(),
            observed_at_epoch_s: None,
            quality: EvidenceQuality::Unknown,
        }
    }

    pub fn kind(&self) -> EvidenceKind {
        self.kind
    }

    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn observed_at_epoch_s(&self) -> Option<u64> {
        self.observed_at_epoch_s
    }

    pub fn quality(&self) -> EvidenceQuality {
        self.quality
    }

    /// Whether this record is coherent enough to support a measured verdict.
    ///
    /// This is deliberately stricter than checking the serialized `kind`.
    /// Imported reports are untrusted input: a measured claim without a value,
    /// a named source, or direct-observation quality fails closed.
    pub fn is_measured(&self) -> bool {
        self.kind == EvidenceKind::Measured
            && self.value.is_some()
            && !self.source.trim().is_empty()
            && matches!(
                self.quality,
                EvidenceQuality::Observed | EvidenceQuality::Validated
            )
    }
}

impl<T: PartialEq> DiagnosticEvidence<T> {
    /// Whether this evidence is measured and describes the value being graded.
    /// This prevents provenance for one observation from being attached to a
    /// different scalar field during report assembly or deserialization.
    pub fn is_measured_for(&self, value: &T) -> bool {
        self.is_measured() && self.value.as_ref() == Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_legacy_evidence_is_explicitly_unavailable() {
        let evidence = DiagnosticEvidence::<u16>::default();
        assert_eq!(evidence.kind(), EvidenceKind::Unavailable);
        assert_eq!(evidence.value(), None);
        assert!(!evidence.is_measured());
        assert_eq!(evidence.quality(), EvidenceQuality::Unknown);
    }

    #[test]
    fn only_measured_constructors_satisfy_measured_requirement() {
        let cases = [
            DiagnosticEvidence::commanded(13_700u16, "setpoint", None),
            DiagnosticEvidence::inferred(13_700u16, "model", None),
            DiagnosticEvidence::unavailable("sensor_missing"),
        ];
        for evidence in cases {
            assert!(!evidence.is_measured());
        }
        assert!(DiagnosticEvidence::measured(13_690u16, "rail_adc", Some(1)).is_measured());
    }

    #[test]
    fn incoherent_serialized_measured_claims_fail_closed() {
        for json in [
            r#"{"kind":"measured","value":13690,"source":"","quality":"observed"}"#,
            r#"{"kind":"measured","value":13690,"source":"   ","quality":"validated"}"#,
            r#"{"kind":"measured","value":null,"source":"rail_adc","quality":"observed"}"#,
            r#"{"kind":"measured","value":13690,"source":"rail_adc","quality":"estimated"}"#,
            r#"{"kind":"measured","value":13690,"source":"rail_adc","quality":"unknown"}"#,
        ] {
            let evidence: DiagnosticEvidence<u16> = serde_json::from_str(json).unwrap();
            assert!(!evidence.is_measured(), "accepted incoherent claim: {json}");
        }
    }

    #[test]
    fn measured_evidence_must_match_the_value_being_graded() {
        let evidence = DiagnosticEvidence::measured(13_690u16, "rail_adc", Some(1));
        assert!(evidence.is_measured_for(&13_690));
        assert!(!evidence.is_measured_for(&13_700));
    }
}
