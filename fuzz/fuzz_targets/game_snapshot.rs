//! A game snapshot a restore or a rollback reads: decoding never panics, and
//! what decodes encodes to bytes that decode to the same encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_behavior::game::GameSnapshot;

fuzz_target!(|data: &[u8]| {
    if let Ok(snapshot) = GameSnapshot::decode(data) {
        let encoded = snapshot.encode();
        let again = GameSnapshot::decode(&encoded).expect("its own encoding decodes");
        assert_eq!(again.encode(), encoded);
    }
});
