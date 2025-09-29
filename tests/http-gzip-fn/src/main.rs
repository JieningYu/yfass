//! Test function for HTTP by g-zipping the data received from client.

use std::{io::Read as _, net::Ipv4Addr};

use axum::{Router, body::Bytes, response::ErrorResponse, routing::post};
use flate2::read::GzEncoder;

fn main() {
    println!("starting http gzip test server");
    let port = std::env::var("YFASS_PORT")
        .expect("missing YFASS_PORT env var")
        .parse::<u16>()
        .unwrap();

    let router: Router<()> = Router::new().route("/", post(accept_http_request));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            axum::serve(listener, router).await.unwrap();
        })
}

async fn accept_http_request(data: Bytes) -> Result<Bytes, ErrorResponse> {
    println!("received {} bytes, compressing..", data.len());
    let mut compressed = Vec::new();
    let mut gz = GzEncoder::new(&data[..], Default::default());
    gz.read_to_end(&mut compressed).map_err(to_err)?;
    println!("compressed into {} bytes, sending..", data.len());
    Ok(compressed.into())
}

#[inline]
fn to_err<E: std::error::Error>(e: E) -> ErrorResponse {
    ErrorResponse::from(e.to_string())
}
