//! A view package, as a dev patch or a build carries one: decoding never
//! panics, and what decodes encodes to bytes that decode to the same encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_ende::{Decode, Encode};
use viso_schema::view::ViewPackage;

fuzz_target!(|data: &[u8]| {
    if let Ok(package) = ViewPackage::decode_from_slice(data) {
        let encoded = package.encode_to_vec();
        let again = ViewPackage::decode_from_slice(&encoded).expect("its own encoding decodes");
        assert_eq!(again.encode_to_vec(), encoded);
    }
});
