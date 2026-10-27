//! A behavior module blob: loading and verifying it never panics, and a
//! module that loads encodes to bytes that load to the same encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_behavior::Module;

fuzz_target!(|data: &[u8]| {
    if let Ok(module) = Module::decode(data) {
        let encoded = module.encode();
        let again = Module::decode(&encoded).expect("its own encoding loads");
        assert_eq!(again.encode(), encoded);
    }
});
