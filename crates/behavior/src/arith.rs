//! Typed scalar arithmetic, casts and text rendering over the value
//! representation.
//!
//! Integers are checked: a result outside its type's range is
//! [`FaultKind::Overflow`], a zero divisor [`FaultKind::DivideByZero`] and a
//! shift amount outside `0..bits` [`FaultKind::ShiftRange`]; bits shifted out of
//! the width are discarded. Floats follow IEEE 754; an `F32` result is rounded to
//! `f32` after every operation, which for `+ - * / %` equals computing in `f32`.

use crate::op::{Arith, ArithOp, DisplayKind, Num};
use crate::value::Value;
use crate::vm::FaultKind;

/// The mathematical value of the integer pattern `v` of type `num`.
#[inline]
fn wide(num: Num, v: i64) -> i128 {
    if num.is_signed() {
        i128::from(v)
    } else {
        i128::from(v as u64)
    }
}

/// `v` as a pattern of type `num`, or `None` if it is out of range.
#[inline]
fn fit(num: Num, v: i128) -> Option<i64> {
    let bits = num.bits();
    let (lo, hi) = if num.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    (lo..=hi).contains(&v).then_some(v as i64)
}

/// `v` truncated to `num`'s width, as a pattern of type `num`.
#[inline]
fn wrap(num: Num, v: i128) -> i64 {
    let shift = 128 - num.bits();
    if num.is_signed() {
        ((v << shift) >> shift) as i64
    } else {
        (((v as u128) << shift) >> shift) as u64 as i64
    }
}

/// `v` rounded to `num`'s float precision.
#[inline]
fn round(num: Num, v: f64) -> f64 {
    if num == Num::F32 { v as f32 as f64 } else { v }
}

#[inline]
fn int(v: &Value) -> Result<i64, FaultKind> {
    v.as_int().ok_or(FaultKind::Internal)
}

#[inline]
fn float(v: &Value) -> Result<f64, FaultKind> {
    v.as_float().ok_or(FaultKind::Internal)
}

/// `a op b`.
pub(crate) fn binary(op: ArithOp, a: &Value, b: &Value) -> Result<Value, FaultKind> {
    let num = op.num();
    if num.is_float() {
        let (x, y) = (float(a)?, float(b)?);
        let r = match op.op() {
            Arith::Add => x + y,
            Arith::Sub => x - y,
            Arith::Mul => x * y,
            Arith::Div => x / y,
            Arith::Rem => x % y,
            Arith::Lt => return Ok(Value::bool(x < y)),
            Arith::Le => return Ok(Value::bool(x <= y)),
            Arith::And | Arith::Or | Arith::Xor | Arith::Shl | Arith::Shr => {
                return Err(FaultKind::Internal);
            }
        };
        return Ok(Value::Float(round(num, r)));
    }
    let (a, b) = (int(a)?, int(b)?);
    let (x, y) = (wide(num, a), wide(num, b));
    let r = match op.op() {
        Arith::Add => x + y,
        Arith::Sub => x - y,
        Arith::Mul => x * y,
        Arith::Div | Arith::Rem => {
            if y == 0 {
                return Err(FaultKind::DivideByZero);
            }
            let q = x / y;
            fit(num, q).ok_or(FaultKind::Overflow)?;
            if op.op() == Arith::Div { q } else { x % y }
        }
        Arith::And => return Ok(Value::Int(a & b)),
        Arith::Or => return Ok(Value::Int(a | b)),
        Arith::Xor => return Ok(Value::Int(a ^ b)),
        Arith::Shl => return Ok(Value::Int(wrap(num, x << shift(num, y)?))),
        Arith::Shr => return Ok(Value::Int(wrap(num, x >> shift(num, y)?))),
        Arith::Lt => return Ok(Value::bool(x < y)),
        Arith::Le => return Ok(Value::bool(x <= y)),
    };
    fit(num, r).map(Value::Int).ok_or(FaultKind::Overflow)
}

/// A shift amount, checked against `num`'s width.
#[inline]
fn shift(num: Num, y: i128) -> Result<u32, FaultKind> {
    if (0..i128::from(num.bits())).contains(&y) {
        Ok(y as u32)
    } else {
        Err(FaultKind::ShiftRange)
    }
}

/// `-v`.
pub(crate) fn neg(num: Num, v: &Value) -> Result<Value, FaultKind> {
    if num.is_float() {
        return Ok(Value::Float(-float(v)?));
    }
    if !num.is_signed() {
        return Err(FaultKind::Internal);
    }
    fit(num, -wide(num, int(v)?))
        .map(Value::Int)
        .ok_or(FaultKind::Overflow)
}

/// The bitwise complement of `v`.
pub(crate) fn bit_not(num: Num, v: &Value) -> Result<Value, FaultKind> {
    if num.is_float() {
        return Err(FaultKind::Internal);
    }
    Ok(Value::Int(wrap(num, !wide(num, int(v)?))))
}

/// `v as to`, with Rust `as` semantics: integers wrap to the target width,
/// floats saturate to the target integer range (`NaN` becomes `0`), and
/// integer-to-float rounds to nearest.
pub(crate) fn cast(from: Num, to: Num, v: &Value) -> Result<Value, FaultKind> {
    Ok(match (from.is_float(), to.is_float()) {
        (false, false) => Value::Int(wrap(to, wide(from, int(v)?))),
        (false, true) => {
            let x = wide(from, int(v)?);
            Value::Float(if to == Num::F32 {
                x as f32 as f64
            } else {
                x as f64
            })
        }
        (true, false) => {
            let x = float(v)?;
            Value::Int(match to {
                Num::I8 => i64::from(x as i8),
                Num::I16 => i64::from(x as i16),
                Num::I32 => i64::from(x as i32),
                Num::I64 => x as i64,
                Num::U8 => i64::from(x as u8),
                Num::U16 => i64::from(x as u16),
                Num::U32 => i64::from(x as u32),
                Num::U64 => x as u64 as i64,
                Num::F32 | Num::F64 => return Err(FaultKind::Internal),
            })
        }
        (true, true) => Value::Float(round(to, float(v)?)),
    })
}

/// The text of `v`: integers in decimal, floats in their shortest
/// round-tripping decimal form (`1`, `0.1`, `inf`, `NaN`), `Char`s as the
/// character. [`DisplayKind::Str`] never allocates and is handled by the
/// interpreter.
pub(crate) fn display(kind: DisplayKind, v: &Value) -> Result<String, FaultKind> {
    Ok(match kind {
        DisplayKind::Bool => if int(v)? != 0 { "true" } else { "false" }.to_owned(),
        DisplayKind::Signed => int(v)?.to_string(),
        DisplayKind::Unsigned => (int(v)? as u64).to_string(),
        DisplayKind::F32 => (float(v)? as f32).to_string(),
        DisplayKind::F64 => float(v)?.to_string(),
        DisplayKind::Char => u32::try_from(int(v)?)
            .ok()
            .and_then(char::from_u32)
            .ok_or(FaultKind::Internal)?
            .to_string(),
        DisplayKind::Str => v.as_str().ok_or(FaultKind::Internal)?.to_owned(),
    })
}

/// The text of the dimensional scalar `v` followed by `suffix`.
pub(crate) fn display_dim(v: &Value, suffix: &str) -> Result<String, FaultKind> {
    Ok(format!("{}{suffix}", float(v)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(op: Arith, num: Num, a: i64, b: i64) -> Result<i64, FaultKind> {
        binary(ArithOp::new(op, num), &Value::Int(a), &Value::Int(b)).map(|v| v.as_int().unwrap())
    }

    #[test]
    fn integer_overflow_is_checked_per_width() {
        assert_eq!(run(Arith::Add, Num::I8, 127, 1), Err(FaultKind::Overflow));
        assert_eq!(run(Arith::Add, Num::I8, 126, 1), Ok(127));
        assert_eq!(run(Arith::Sub, Num::U8, 0, 1), Err(FaultKind::Overflow));
        assert_eq!(run(Arith::Mul, Num::U64, -1, 1), Ok(-1));
        assert_eq!(run(Arith::Add, Num::U64, -1, 1), Err(FaultKind::Overflow));
        assert_eq!(
            run(Arith::Div, Num::I32, i32::MIN.into(), -1),
            Err(FaultKind::Overflow)
        );
        assert_eq!(
            run(Arith::Rem, Num::I64, i64::MIN, -1),
            Err(FaultKind::Overflow)
        );
        assert_eq!(
            run(Arith::Rem, Num::I64, 7, 0),
            Err(FaultKind::DivideByZero)
        );
    }

    #[test]
    fn shifts_check_the_amount_and_truncate() {
        assert_eq!(run(Arith::Shl, Num::U8, 0x81, 1), Ok(0x02));
        assert_eq!(run(Arith::Shl, Num::I8, 0x40, 1), Ok(-128));
        assert_eq!(run(Arith::Shr, Num::I8, -128, 7), Ok(-1));
        assert_eq!(run(Arith::Shr, Num::U64, -1, 63), Ok(1));
        assert_eq!(run(Arith::Shl, Num::I32, 1, 32), Err(FaultKind::ShiftRange));
        assert_eq!(run(Arith::Shl, Num::I32, 1, -1), Err(FaultKind::ShiftRange));
    }

    #[test]
    fn unsigned_ordering_uses_the_full_width() {
        assert_eq!(run(Arith::Lt, Num::U64, 1, -1), Ok(1));
        assert_eq!(run(Arith::Lt, Num::I64, 1, -1), Ok(0));
    }

    #[test]
    fn casts_follow_rust_as() {
        let int = |from, to, v: i64| cast(from, to, &Value::Int(v)).unwrap();
        assert_eq!(int(Num::I64, Num::U8, 300), Value::Int(44));
        assert_eq!(int(Num::I64, Num::I8, 200), Value::Int(-56));
        assert_eq!(int(Num::I8, Num::U64, -1), Value::Int(-1));
        assert_eq!(int(Num::U64, Num::F64, -1), Value::Float(u64::MAX as f64));
        let float = |to, v: f64| cast(Num::F64, to, &Value::Float(v)).unwrap();
        assert_eq!(float(Num::U8, 300.5), Value::Int(255));
        assert_eq!(float(Num::I32, f64::NAN), Value::Int(0));
        assert_eq!(float(Num::F32, 0.1), Value::Float(0.1f32 as f64));
    }

    #[test]
    fn f32_results_round_to_f32() {
        let r = binary(
            ArithOp::new(Arith::Add, Num::F32),
            &Value::Float(0.1f32 as f64),
            &Value::Float(0.2f32 as f64),
        )
        .unwrap();
        assert_eq!(r, Value::Float((0.1f32 + 0.2f32) as f64));
    }

    #[test]
    fn display_renders_each_kind() {
        let show = |kind, v| display(kind, &v).unwrap();
        assert_eq!(show(DisplayKind::Bool, Value::Int(1)), "true");
        assert_eq!(
            show(DisplayKind::Unsigned, Value::Int(-1)),
            u64::MAX.to_string()
        );
        assert_eq!(show(DisplayKind::F64, Value::Float(1.0)), "1");
        assert_eq!(show(DisplayKind::F32, Value::Float(0.1f32 as f64)), "0.1");
        assert_eq!(show(DisplayKind::Char, Value::Int('é' as i64)), "é");
        assert_eq!(display_dim(&Value::Float(12.5), "dp").unwrap(), "12.5dp");
    }
}
