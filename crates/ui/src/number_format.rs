//! How a number field reads, shows and steps the number its text holds.
//!
//! A number field's text is the number: typing changes the text, and the
//! number follows whenever the text reads as one inside the field's range.
//! Stepping (an arrow key, the wheel, a scrub) and settling (Enter, the focus
//! leaving) rewrite the text from the number, kept within the range and
//! rounded to the field's precision, so what it shows is always what it
//! holds. A value given from outside replaces the text only when the text no
//! longer shows as it at the precision, so a half-typed `1.` survives the
//! value `1` coming back.

use viso_runtime::InputKind;
pub use viso_schema::number::NumberFormat;

/// What a number field of `format` holds, which decides what it takes and
/// the keyboard it shows: digits alone for whole numbers from zero up, a
/// sign besides for whole numbers below, a decimal point besides for
/// fractions.
pub fn number_kind(format: &NumberFormat) -> InputKind {
    match (format.whole(), format.signed()) {
        (true, false) => InputKind::Digits,
        (true, true) => InputKind::Integer,
        (false, _) => InputKind::Decimal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_field_holds_digits_a_sign_or_a_point() {
        assert_eq!(
            number_kind(&NumberFormat::new(0.0, 10.0, 1.0)),
            InputKind::Digits
        );
        assert_eq!(number_kind(&NumberFormat::default()), InputKind::Integer);
        assert_eq!(
            number_kind(&NumberFormat::new(0.0, 1.0, 0.1)),
            InputKind::Decimal
        );
    }
}
