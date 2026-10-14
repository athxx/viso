//! A binary input tape, as `viso test game --tape` reads one: decoding never
//! panics, and what decodes encodes to bytes that decode to the same
//! encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_behavior::game::InputTape;

fuzz_target!(|data: &[u8]| {
    if let Ok(tape) = InputTape::decode(data) {
        let encoded = tape.encode();
        let again = InputTape::decode(&encoded).expect("its own encoding decodes");
        assert_eq!(again.encode(), encoded);
    }
});
