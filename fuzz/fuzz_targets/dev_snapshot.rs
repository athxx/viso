//! A dev snapshot a restore hands the app: decoding never panics, and what
//! decodes encodes to bytes that decode to the same encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_view::dev::snapshot::DevSnapshot;

fuzz_target!(|data: &[u8]| {
    if let Ok(snapshot) = DevSnapshot::decode(data) {
        let encoded = snapshot.encode();
        let again = DevSnapshot::decode(&encoded).expect("its own encoding decodes");
        assert_eq!(again.encode(), encoded);
    }
});
