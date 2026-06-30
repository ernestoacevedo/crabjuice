//! Core types shared across crabjuice crates.

use core::fmt;

/// Result type used by crabjuice APIs.
pub type Result<T> = core::result::Result<T, Error>;

/// Common errors returned by crabjuice APIs.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// A parameter range was invalid.
    InvalidParameterRange { min: f32, max: f32 },
    /// A parameter value was outside the allowed range.
    ParameterOutOfRange { value: f32, min: f32, max: f32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameterRange { min, max } => {
                write!(
                    f,
                    "invalid parameter range: min {min} must be less than max {max}"
                )
            }
            Self::ParameterOutOfRange { value, min, max } => {
                write!(f, "parameter value {value} is outside range {min}..={max}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Stable identifier for a processor parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParameterId(&'static str);

impl ParameterId {
    /// Creates a new parameter identifier.
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    /// Returns the identifier as a string slice.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ParameterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Floating-point parameter with linear normalized conversion.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatParameter {
    id: ParameterId,
    min: f32,
    max: f32,
    default: f32,
    value: f32,
}

impl FloatParameter {
    /// Creates a parameter with `default` as its current value.
    pub fn new(id: ParameterId, min: f32, max: f32, default: f32) -> Result<Self> {
        if !min.is_finite() || !max.is_finite() || min >= max {
            return Err(Error::InvalidParameterRange { min, max });
        }

        if !default.is_finite() || default < min || default > max {
            return Err(Error::ParameterOutOfRange {
                value: default,
                min,
                max,
            });
        }

        Ok(Self {
            id,
            min,
            max,
            default,
            value: default,
        })
    }

    /// Returns the parameter identifier.
    pub const fn id(&self) -> ParameterId {
        self.id
    }

    /// Returns the minimum plain value.
    pub const fn min(&self) -> f32 {
        self.min
    }

    /// Returns the maximum plain value.
    pub const fn max(&self) -> f32 {
        self.max
    }

    /// Returns the default plain value.
    pub const fn default(&self) -> f32 {
        self.default
    }

    /// Returns the current plain value.
    pub const fn value(&self) -> f32 {
        self.value
    }

    /// Sets the current plain value.
    pub fn set_value(&mut self, value: f32) -> Result<()> {
        if !value.is_finite() || value < self.min || value > self.max {
            return Err(Error::ParameterOutOfRange {
                value,
                min: self.min,
                max: self.max,
            });
        }

        self.value = value;
        Ok(())
    }

    /// Returns the current value normalized to `0.0..=1.0`.
    pub fn normalized_value(&self) -> f32 {
        self.value_to_normalized(self.value)
    }

    /// Converts a plain value to a normalized `0.0..=1.0` value.
    pub fn value_to_normalized(&self, value: f32) -> f32 {
        ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// Converts a normalized `0.0..=1.0` value to a plain value.
    pub fn normalized_to_value(&self, normalized: f32) -> f32 {
        let normalized = normalized.clamp(0.0, 1.0);
        self.min + normalized * (self.max - self.min)
    }

    /// Sets the current value from a normalized `0.0..=1.0` value.
    pub fn set_normalized_value(&mut self, normalized: f32) {
        self.value = self.normalized_to_value(normalized);
    }

    /// Restores the current value to the default.
    pub fn reset_to_default(&mut self) {
        self.value = self.default;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_parameter_converts_normalized_values() {
        let mut gain =
            FloatParameter::new(ParameterId::new("gain"), -60.0, 12.0, 0.0).expect("valid range");

        assert_eq!(gain.normalized_to_value(0.0), -60.0);
        assert_eq!(gain.normalized_to_value(1.0), 12.0);
        assert_eq!(gain.value_to_normalized(-24.0), 0.5);

        gain.set_normalized_value(0.25);
        assert_eq!(gain.value(), -42.0);

        gain.reset_to_default();
        assert_eq!(gain.value(), 0.0);
    }

    #[test]
    fn float_parameter_rejects_invalid_values() {
        assert!(FloatParameter::new(ParameterId::new("x"), 1.0, 1.0, 1.0).is_err());
        assert!(FloatParameter::new(ParameterId::new("x"), 0.0, 1.0, 2.0).is_err());
    }
}
