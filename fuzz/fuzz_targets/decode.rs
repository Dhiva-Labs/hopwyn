#![no_main]

use hopwyn_proto::Message;
use libfuzzer_sys::fuzz_target;

// Any input must either fail cleanly or decode to a message that
// re-encodes to exactly the same bytes.
fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = Message::decode(data) {
        assert_eq!(msg.to_bytes(), data);
    }
});
