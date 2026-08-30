//! Typed-number questions, and the arithmetic that grades them.
//!
//! # Why this format exists
//!
//! Two reasons, and the second is the one that matters.
//!
//! **It is what "trends, not tenths" looks like as data.** A multiple-choice
//! question about a figure is graded on whether you recognised the exact
//! printed value among four. That is a verbatim test, and verbatim recall is
//! not the thing worth training: what is worth training is holding a figure
//! accurately enough to use it. A tolerance band says so explicitly — the
//! precision demanded is a field on the question, not an accident of how the
//! options were written.
//!
//! **It removes the app's ability to teach you false numbers.** A multiple-
//! choice question about a statistic needs three wrong statistics, and a model
//! asked for plausible wrong statistics invents them. Reading a plausible
//! invented figure and deliberating over it is how it gets remembered:
//! multiple-choice lures persist as later intrusions, and the mechanism is
//! reconstruction rather than familiarity, so "I'll just remember it was the
//! wrong one" is not a defence. A typed answer has no lures. There is nothing
//! to implant.
//!
//! # Everything here is deterministic
//!
//! Grading is `|given − value| ≤ tolerance`. No model call, at grade time or
//! ever. That is not an optimisation — a grader that is a model call is an
//! instrument that drifts, and a score history whose instrument drifts cannot
//! be compared with itself. The tolerance is decided once, when the question is
//! written, and stored.

use serde::{Deserialize, Serialize};

/// The answer to a numeric question, with the precision it demands.
///
/// **Never serialized to the browser before submission.** It is the answer key.
/// See `crate::model::PublicQuestion`, which carries the tolerance and the unit
/// — which are hints about precision, not about the value — and no `value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericAnswer {
    /// The figure as printed in the document, in whatever unit `unit` names.
    pub value: f64,
    /// Half-width of the accepted band, in the same unit. Strictly positive:
    /// see [`NumericAnswer::validate`] for why a zero tolerance is refused
    /// rather than treated as "exact".
    pub tolerance: f64,
    /// `"%"`, `"$B"`, `"bps"`. Display only — never parsed, never compared.
    /// Empty when the figure is a bare count.
    #[serde(default)]
    pub unit: String,
}

/// The narrowest tolerance a question may demand, as a fraction of the figure.
///
/// A model left to choose freely will ask for the printed value to the tenth,
/// because that is what "correct" means to it. That is precisely the test this
/// format exists to *not* be. One percent is the floor.
pub const MIN_TOLERANCE_FRACTION: f64 = 0.01;

/// The widest tolerance a question may demand, as a fraction of the figure.
///
/// Past about a quarter the band swallows every answer anyone would plausibly
/// give, and the question stops distinguishing having read the document from
/// not having read it.
pub const MAX_TOLERANCE_FRACTION: f64 = 0.25;

/// Absolute floors for the two fractions above, so a figure near zero still
/// gets a usable band. Without these, `value: 0.0` admits only `tolerance: 0.0`,
/// which is rejected, and every question about a zero figure fails validation.
pub const MIN_TOLERANCE_ABSOLUTE: f64 = 0.005;
pub const MAX_TOLERANCE_ABSOLUTE: f64 = 0.05;

/// Beyond this the figure is not a statistic from a document, it is a
/// misplaced exponent. Bounded because an unbounded float reaches the browser
/// as `1e300` and renders as gibberish.
pub const MAX_ABSOLUTE_VALUE: f64 = 1e15;

/// Longest unit label. Long enough for `"$ billions"`, short enough that it
/// cannot smuggle a sentence — or the answer — into the quiz payload.
pub const UNIT_MAX_LEN: usize = 12;

impl NumericAnswer {
    /// Is `given` inside the band?
    ///
    /// The slack is float housekeeping, not generosity. A reader who types the
    /// exact edge of the band — `3.0` against `value: 4.0, tolerance: 1.0` —
    /// must be marked correct, and `4.0 - 3.0 <= 1.0` is not guaranteed to hold
    /// in binary floating point once the values come back through JSON and
    /// DynamoDB's decimal string encoding.
    pub fn accepts(&self, given: f64) -> bool {
        if !given.is_finite() {
            return false;
        }
        let slack = self.tolerance.abs() * 1e-9 + 1e-9;
        (given - self.value).abs() <= self.tolerance + slack
    }

    /// Everything about this answer that must be true before it is stored.
    ///
    /// Called at generation time on model output, which is untrusted input. The
    /// failure mode this prevents is not a crash — it is a question that is
    /// silently impossible (`tolerance: 0`, so only one float in the universe
    /// is accepted) or silently free (`tolerance: 1e9`), either of which
    /// produces a score that looks like a measurement and is not.
    pub fn validate(&self) -> Result<(), String> {
        if !self.value.is_finite() {
            return Err("the answer is not a finite number".into());
        }
        if self.value.abs() > MAX_ABSOLUTE_VALUE {
            return Err(format!(
                "the answer {} is larger than any figure in a document",
                self.value
            ));
        }
        if !self.tolerance.is_finite() || self.tolerance <= 0.0 {
            return Err("the tolerance must be a positive number".into());
        }

        let (min, max) = self.permitted_tolerance();
        if self.tolerance < min {
            return Err(format!(
                "a tolerance of {} demands the figure to the decimal; the floor is {min}",
                self.tolerance
            ));
        }
        if self.tolerance > max {
            return Err(format!(
                "a tolerance of {} accepts almost any answer; the ceiling is {max}",
                self.tolerance
            ));
        }

        let unit_len = self.unit.chars().count();
        if unit_len > UNIT_MAX_LEN {
            return Err(format!("the unit is {unit_len} characters"));
        }
        if self.unit.chars().any(char::is_control) {
            return Err("the unit contains a control character".into());
        }

        Ok(())
    }

    /// The `(min, max)` tolerance permitted for this figure.
    ///
    /// Both are fractions of the magnitude with an absolute floor, so a figure
    /// of `0.0` — a real thing for a net change — still has a usable band
    /// rather than an empty one.
    fn permitted_tolerance(&self) -> (f64, f64) {
        let magnitude = self.value.abs();
        (
            (MIN_TOLERANCE_FRACTION * magnitude).max(MIN_TOLERANCE_ABSOLUTE),
            (MAX_TOLERANCE_FRACTION * magnitude).max(MAX_TOLERANCE_ABSOLUTE),
        )
    }
}

/// Turn what the reader typed into a number, or `None`.
///
/// # What is forgiven, and why
///
/// The reader is answering from memory on a phone, and the figure they are
/// recalling was printed in a financial document. So the shapes that document
/// uses are accepted: thousands separators, a leading currency symbol, a
/// trailing percent sign, and accounting parentheses for negatives — `(1.2)` is
/// `-1.2` in every table this app will ever see. The Unicode minus is accepted
/// because iOS substitutes it.
///
/// # What is not
///
/// Anything left over after that. `"about 4"`, `"4 or 5"`, `"-4 to -5"` all
/// return `None` rather than being guessed at. A range is not an answer to a
/// question that already states its own tolerance, and silently reading the
/// first number out of `"4 or 5"` would mark a hedge correct — which is exactly
/// the habit the confidence bands exist to price honestly instead.
///
/// `None` is graded as wrong, not as unanswered. The client blocks submission
/// on an unparseable entry, so reaching the server means the client was bypassed
/// or is stale, and in both cases refusing to grade the whole quiz is worse.
pub fn parse_reader_value(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Accounting negatives. Stripped first so the sign handling below sees the
    // bare number, and `(-1.2)` — belt and braces from a reader — still works.
    let (body, parenthesised) = match trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        Some(inner) => (inner.trim(), true),
        None => (trimmed, false),
    };

    let mut cleaned = String::with_capacity(body.len());
    let mut seen_digit = false;
    let mut seen_dot = false;

    for (i, ch) in body.chars().enumerate() {
        match ch {
            // A sign is only a sign in first position. `4-5` is a range, not a
            // number, and must not collapse to `45`.
            '-' | '\u{2212}' if i == 0 => cleaned.push('-'),
            '+' if i == 0 => {}
            // Ignored wherever they appear: no document writes `1,2` meaning
            // one-point-two, and rejecting a stray separator would fail an
            // answer that is otherwise unambiguous.
            ',' | '_' | ' ' | '\u{00a0}' | '\u{202f}' => {}
            // Unit noise the reader echoed back. Only ever stripped, never
            // compared against the question's own unit — a reader who types
            // "4%" on a question denominated in points has still said 4.
            '$' | '%' => {}
            '.' => {
                // A second dot means a date or a version, not a figure.
                if seen_dot {
                    return None;
                }
                seen_dot = true;
                cleaned.push('.');
            }
            c if c.is_ascii_digit() => {
                seen_digit = true;
                cleaned.push(c);
            }
            // Anything else — a letter, a slash, a second sign — is a phrase,
            // and a phrase is not a number.
            _ => return None,
        }
    }

    if !seen_digit {
        return None;
    }

    let parsed: f64 = cleaned.parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }

    Some(if parenthesised && parsed > 0.0 {
        -parsed
    } else {
        parsed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(value: f64, tolerance: f64) -> NumericAnswer {
        NumericAnswer {
            value,
            tolerance,
            unit: "%".into(),
        }
    }

    #[test]
    fn the_band_is_inclusive_at_both_edges() {
        let a = answer(-4.0, 1.0);
        assert!(a.accepts(-3.0), "the upper edge is inside the band");
        assert!(a.accepts(-5.0), "the lower edge is inside the band");
        assert!(a.accepts(-4.0));
        assert!(!a.accepts(-2.9));
        assert!(!a.accepts(-5.1));
    }

    /// The edge cases are the point: a reader who types the exact boundary is
    /// right, and float arithmetic must not decide otherwise.
    #[test]
    fn edges_survive_arithmetic_that_is_not_exact() {
        let a = answer(0.3, 0.1);
        assert!(a.accepts(0.2), "0.3 - 0.1 is not exactly 0.2 in binary");
        assert!(a.accepts(0.4));
    }

    #[test]
    fn a_reader_may_type_a_figure_the_way_a_document_prints_it() {
        assert_eq!(parse_reader_value("-4.0"), Some(-4.0));
        assert_eq!(parse_reader_value(" -4.0 % "), Some(-4.0));
        assert_eq!(parse_reader_value("$1,250"), Some(1250.0));
        assert_eq!(parse_reader_value("1 250.5"), Some(1250.5));
        // Accounting negatives, which every financial table uses.
        assert_eq!(parse_reader_value("(1.2)"), Some(-1.2));
        assert_eq!(parse_reader_value("(-1.2)"), Some(-1.2));
        // iOS substitutes the Unicode minus.
        assert_eq!(parse_reader_value("\u{2212}4"), Some(-4.0));
        assert_eq!(parse_reader_value("+7"), Some(7.0));
    }

    /// A hedge is not an answer. Reading the first number out of one would mark
    /// it correct, which prices hedging at zero — the confidence bands are what
    /// price uncertainty, and they only work if the answer itself is committed.
    #[test]
    fn a_phrase_or_a_range_is_not_a_number() {
        for hedge in [
            "about 4", "4 or 5", "-4 to -5", "4-5", "four", "", "   ", "%", "-", "1.2.3", "1/2",
            "4e5",
        ] {
            assert_eq!(
                parse_reader_value(hedge),
                None,
                "{hedge:?} parsed as a number"
            );
        }
    }

    #[test]
    fn a_tolerance_that_demands_the_decimal_is_refused() {
        // 0.001 on a figure of 4.0 is 0.025% — the verbatim test this format
        // exists not to be.
        assert!(answer(4.0, 0.001).validate().is_err());
    }

    #[test]
    fn a_tolerance_that_accepts_anything_is_refused() {
        assert!(answer(4.0, 3.0).validate().is_err());
        assert!(answer(4.0, 0.0).validate().is_err());
        assert!(answer(4.0, -1.0).validate().is_err());
    }

    #[test]
    fn a_sensible_tolerance_is_accepted() {
        answer(-4.0, 1.0).validate().expect("25% of 4.0");
        answer(1250.0, 50.0).validate().expect("4% of 1250");
        // A figure of zero is real — a net change — and must still be askable.
        answer(0.0, 0.02)
            .validate()
            .expect("absolute floor applies");
    }

    #[test]
    fn nonsense_values_are_refused() {
        assert!(answer(f64::NAN, 1.0).validate().is_err());
        assert!(answer(f64::INFINITY, 1.0).validate().is_err());
        assert!(answer(1e20, 1e18).validate().is_err());
        assert!(!answer(4.0, 1.0).accepts(f64::NAN));
    }

    #[test]
    fn an_over_long_unit_is_refused() {
        let mut a = answer(4.0, 1.0);
        a.unit = "x".repeat(UNIT_MAX_LEN + 1);
        assert!(a.validate().is_err());
        a.unit = "\u{7}".into();
        assert!(a.validate().is_err());
    }
}
