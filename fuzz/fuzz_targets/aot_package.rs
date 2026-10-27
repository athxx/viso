//! An AOT view package, as a build embeds one: decoding never panics, and
//! what decodes encodes to bytes that decode to the same encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_ende::{Decode, Encode};
use viso_schema::aot::AotPackage;

fuzz_target!(|data: &[u8]| {
    if let Ok(package) = AotPackage::decode_from_slice(data) {
        let encoded = package.encode_to_vec();
        let again = AotPackage::decode_from_slice(&encoded).expect("its own encoding decodes");
        assert_eq!(again.encode_to_vec(), encoded);
    }
});
