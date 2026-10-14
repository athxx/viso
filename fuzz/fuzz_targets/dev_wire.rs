//! A dev frame's body, from the host or from a runtime: decoding never
//! panics, and what decodes encodes to bytes that decode to the same
//! encoding. The same bytes read as a stream of frames never panic either.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_view::dev::wire::{HostMessage, Message, RuntimeMessage, encode, read_frame};

fn canonical<M: Message>(body: &[u8]) {
    if let Ok(message) = M::from_frame(body) {
        // Compared encoded, so a NaN the input carries still matches.
        let encoded = encode(&message);
        let again = M::from_frame(&encoded).expect("its own encoding decodes");
        assert_eq!(encode(&again), encoded);
    }
}

fuzz_target!(|data: &[u8]| {
    canonical::<HostMessage>(data);
    canonical::<RuntimeMessage>(data);
    let mut buf = Vec::new();
    let mut stream = data;
    while let Ok(Some(_)) = read_frame::<RuntimeMessage>(&mut stream, &mut buf) {}
});
