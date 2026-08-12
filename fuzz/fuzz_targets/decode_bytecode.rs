#![no_main]

use libfuzzer_sys::fuzz_target;
use rocksgraph::fuzzing_exports::decode;

fuzz_target!(|data: &[u8]| {
    // We do not care if it returns a StoreError (expected for bad data),
    // but it must never panic, hang, or OOM on malformed arbitrary bytes.
    let _ = decode(data);
});
