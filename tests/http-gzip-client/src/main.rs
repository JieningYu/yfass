//! Test client for `test-http-gzip-fn`.

use std::io::Read as _;

use flate2::bufread::GzDecoder;

const MATERIAL: &[u8] = include_bytes!("../../material.webp");

fn main() {
    let host = std::env::var("YFASS_HOST").expect("missing YFASS_HOST env var");

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("http://{host}/"))
        .body(MATERIAL)
        .send()
        .expect("request failed");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "bad status code");

    let compressed = resp.bytes().expect("cannot read response body");
    assert_ne!(compressed.len(), 0, "empty data");

    let mut decompressed = Vec::new();
    let mut gz = GzDecoder::new(&compressed[..]);
    gz.read_to_end(&mut decompressed)
        .expect("decompression failed");
    assert_eq!(decompressed.len(), MATERIAL.len(), "invalid length");
    assert!(decompressed == MATERIAL, "non-identical data");
}
