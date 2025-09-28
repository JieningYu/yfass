//! Test client for `test-ws-gzip-fn`.

use std::io::Read as _;

use flate2::bufread::GzDecoder;
use tungstenite::Message;

const MATERIAL: &[u8] = include_bytes!("../material.webp");

fn main() {
    let host = std::env::var("YFASS_HOST").expect("missing YFASS_HOST env var");

    let (mut ws, _) = tungstenite::connect(format!("ws://{}/", host)).expect("connect failed");
    assert!(ws.can_write(), "cannot write");
    ws.write(Message::Binary(MATERIAL.into()))
        .expect("write material failed");
    ws.flush().unwrap();
    let Message::Binary(compressed) = ws.read().expect("cannot read") else {
        panic!("invalid message type")
    };
    assert_ne!(compressed.len(), 0, "empty data");
    ws.close(None).expect("cannot close connection");
    ws.flush().unwrap();

    let mut decompressed = Vec::new();
    let mut gz = GzDecoder::new(&compressed[..]);
    gz.read_to_end(&mut decompressed)
        .expect("decompression failed");
    assert_eq!(decompressed.len(), MATERIAL.len(), "invalid length");
    assert!(decompressed == MATERIAL, "non-identical data");
}
