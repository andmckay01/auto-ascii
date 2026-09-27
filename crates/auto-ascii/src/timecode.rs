//! Shared timestamp parsing and formatting.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimecodeError {
    TooManyFields,
    BadField(String),
    OutOfRange(String),
}

impl fmt::Display for TimecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimecodeError::TooManyFields => {
                write!(f, "expected SECONDS, MM:SS or HH:MM:SS")
            }
            TimecodeError::BadField(p) => write!(f, "bad timestamp component {p:?}"),
            TimecodeError::OutOfRange(p) => {
                write!(f, "timestamp component {p:?} must be finite and >= 0")
            }
        }
    }
}

impl std::error::Error for TimecodeError {}

pub fn parse(s: &str) -> Result<f64, TimecodeError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return Err(TimecodeError::TooManyFields);
    }
    let mut secs = 0.0f64;
    for p in &parts {
        let v: f64 =
            p.trim().parse().map_err(|_| TimecodeError::BadField((*p).to_string()))?;
        if !v.is_finite() || v < 0.0 {
            return Err(TimecodeError::OutOfRange((*p).to_string()));
        }
        secs = secs * 60.0 + v;
    }
    Ok(secs)
}

pub fn format_mmss(secs: f64) -> String {
    let secs = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_shapes() {
        assert_eq!(parse("42").unwrap(), 42.0);
        assert_eq!(parse("42.5").unwrap(), 42.5);
        assert_eq!(parse("0").unwrap(), 0.0);
        assert_eq!(parse("1:30").unwrap(), 90.0);
        assert_eq!(parse("0:01:30.5").unwrap(), 90.5);
        assert_eq!(parse("2:00:00").unwrap(), 7200.0);
        assert_eq!(parse("0:90").unwrap(), 90.0);
        assert_eq!(parse(" 1 : 30 ").unwrap(), 90.0);
    }

    #[test]
    fn rejects_with_the_right_reason() {
        assert_eq!(parse("1:2:3:4"), Err(TimecodeError::TooManyFields));
        assert_eq!(parse(""), Err(TimecodeError::BadField(String::new())));
        assert_eq!(parse("abc"), Err(TimecodeError::BadField("abc".into())));
        assert_eq!(parse("1::30"), Err(TimecodeError::BadField(String::new())));
        assert_eq!(parse("-5"), Err(TimecodeError::OutOfRange("-5".into())));
        assert_eq!(parse("inf"), Err(TimecodeError::OutOfRange("inf".into())));
        assert_eq!(parse("1:NaN"), Err(TimecodeError::OutOfRange("NaN".into())));
        assert_eq!(
            parse("1:2:3:4").unwrap_err().to_string(),
            "expected SECONDS, MM:SS or HH:MM:SS"
        );
        assert_eq!(parse("abc").unwrap_err().to_string(), "bad timestamp component \"abc\"");
    }

    #[test]
    fn formats_like_the_progress_overlay() {
        assert_eq!(format_mmss(0.0), "0:00");
        assert_eq!(format_mmss(9.99), "0:09");
        assert_eq!(format_mmss(90.9), "1:30");
        assert_eq!(format_mmss(3599.0), "59:59");
        assert_eq!(format_mmss(3600.0), "1:00:00");
        assert_eq!(format_mmss(3661.0), "1:01:01");
        assert_eq!(format_mmss(-1.0), "0:00");
        assert_eq!(format_mmss(f64::NAN), "0:00");
    }

    #[test]
    fn format_round_trips_through_parse() {
        for secs in [0u64, 7, 59, 60, 61, 599, 3599, 3600, 3661, 86_399] {
            let text = format_mmss(secs as f64);
            assert_eq!(parse(&text).unwrap(), secs as f64, "round trip of {text:?}");
        }
    }
}
