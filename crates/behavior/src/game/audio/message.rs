//! A message to the audio thread as plain data: a value of the package's
//! `@derive(AudioCommand)` enum, flattened into a fixed array of tokens so it
//! crosses a [`realtime_queue`](super::realtime_queue) by copy. The audio
//! thread decodes it into a value it reuses, so a warm decode allocates
//! nothing.

use std::rc::Rc;

use crate::value::{Aggregate, Value};

/// The most tokens a message flattens into: a scalar is one, a variant with
/// a payload or a record one plus its fields'.
pub const MESSAGE_TOKENS: usize = 16;

const INT: u8 = 0;
const FLOAT: u8 = 1;
const AGG: u8 = 2;

/// A flattened message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioMessage {
    len: u8,
    kinds: [u8; MESSAGE_TOKENS],
    words: [u64; MESSAGE_TOKENS],
}

impl AudioMessage {
    /// `value` flattened: `None` when it holds anything but integers,
    /// floats and aggregates of them, or flattens into more than
    /// [`MESSAGE_TOKENS`] tokens.
    pub fn encode(value: &Value) -> Option<AudioMessage> {
        let mut message = AudioMessage {
            len: 0,
            kinds: [0; MESSAGE_TOKENS],
            words: [0; MESSAGE_TOKENS],
        };
        message.push_value(value)?;
        Some(message)
    }

    fn push(&mut self, kind: u8, word: u64) -> Option<()> {
        let at = usize::from(self.len);
        if at == MESSAGE_TOKENS {
            return None;
        }
        self.kinds[at] = kind;
        self.words[at] = word;
        self.len += 1;
        Some(())
    }

    fn push_value(&mut self, value: &Value) -> Option<()> {
        match value {
            Value::Int(n) => self.push(INT, *n as u64),
            Value::Float(x) => self.push(FLOAT, x.to_bits()),
            Value::Agg(agg) => {
                let fields = u32::try_from(agg.fields.len()).ok()?;
                self.push(AGG, u64::from(agg.tag) << 32 | u64::from(fields))?;
                agg.fields.iter().try_for_each(|f| self.push_value(f))
            }
            _ => None,
        }
    }
}

/// Where the audio thread decodes messages: the last value of each variant,
/// whose aggregates the next message of that variant overwrites in place
/// when nothing else holds them and they have its shape, so a warm decode
/// allocates and frees nothing.
#[derive(Debug, Default)]
pub struct MessageSlot {
    /// The last value of each variant with a payload, by its tag.
    variants: Vec<Value>,
    /// The last value without one.
    scalar: Value,
}

impl MessageSlot {
    /// The value `message` flattens.
    pub fn decode(&mut self, message: &AudioMessage) -> &Value {
        let mut at = 0;
        let value = if message.kinds[0] == AGG {
            let tag = (message.words[0] >> 32) as usize;
            if self.variants.len() <= tag {
                self.variants.resize(tag + 1, Value::Nil);
            }
            &mut self.variants[tag]
        } else {
            &mut self.scalar
        };
        fill(value, message, &mut at);
        value
    }
}

fn fill(value: &mut Value, message: &AudioMessage, at: &mut usize) {
    let (kind, word) = (message.kinds[*at], message.words[*at]);
    *at += 1;
    match kind {
        INT => *value = Value::Int(word as i64),
        FLOAT => *value = Value::Float(f64::from_bits(word)),
        _ => {
            let (tag, len) = ((word >> 32) as u32, word as u32 as usize);
            if let Value::Agg(agg) = value
                && let Some(agg) = Rc::get_mut(agg)
                && agg.fields.len() == len
            {
                agg.tag = tag;
                for field in &mut agg.fields {
                    fill(field, message, at);
                }
                return;
            }
            let mut fields = vec![Value::Nil; len].into_boxed_slice();
            for field in &mut fields {
                fill(field, message, at);
            }
            *value = Value::Agg(Rc::new(Aggregate { tag, fields }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(tag: u32, fields: Vec<Value>) -> Value {
        Value::Agg(Rc::new(Aggregate {
            tag,
            fields: fields.into(),
        }))
    }

    #[test]
    fn a_message_round_trips_and_reuses_its_value() {
        let note = agg(
            1,
            vec![
                Value::Int(60),
                agg(0, vec![Value::Float(0.5), Value::Int(1)]),
            ],
        );
        let message = AudioMessage::encode(&note).expect("plain");
        let mut slot = MessageSlot::default();
        assert_eq!(slot.decode(&message), &note);
        let Value::Agg(first) = slot.decode(&message).clone() else {
            panic!("an aggregate")
        };
        let held = Rc::as_ptr(&first);
        drop(first);
        let other = agg(
            1,
            vec![
                Value::Int(62),
                agg(0, vec![Value::Float(0.25), Value::Int(0)]),
            ],
        );
        let decoded = slot.decode(&AudioMessage::encode(&other).expect("plain"));
        assert_eq!(decoded, &other);
        let Value::Agg(again) = decoded else {
            panic!("an aggregate")
        };
        assert_eq!(Rc::as_ptr(again), held, "overwritten in place");
        let _ = slot.decode(&AudioMessage::encode(&Value::Int(0)).expect("plain"));
        let Value::Agg(kept) = slot.decode(&message) else {
            panic!("an aggregate")
        };
        assert_eq!(Rc::as_ptr(kept), held, "kept across a unit variant");
        assert_eq!(
            slot.decode(&AudioMessage::encode(&Value::Int(3)).expect("plain")),
            &Value::Int(3),
            "a unit variant"
        );
    }

    #[test]
    fn only_plain_data_fits_a_message() {
        assert_eq!(AudioMessage::encode(&Value::str("a")), None);
        let wide = agg(0, vec![Value::Int(0); MESSAGE_TOKENS]);
        assert_eq!(AudioMessage::encode(&wide), None, "one token too many");
        let fits = agg(0, vec![Value::Int(0); MESSAGE_TOKENS - 1]);
        assert!(AudioMessage::encode(&fits).is_some());
    }
}
