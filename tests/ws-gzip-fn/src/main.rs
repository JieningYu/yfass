//! Test function for websocket by g-zipping the data received from client.

use std::{io::Read as _, net::Ipv4Addr};

use axum::{
    Router,
    extract::{WebSocketUpgrade, ws::Message},
    response::Response,
    routing::any,
};
use flate2::read::GzEncoder;
use futures_util::{StreamExt as _, TryStreamExt as _};

fn main() {
    println!("starting websocket gzip test server");
    let port = std::env::var("YFASS_PORT")
        .expect("missing YFASS_PORT env var")
        .parse::<u16>()
        .unwrap();

    let router: Router<()> = Router::new().route("/", any(accept_ws_request));

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

async fn accept_ws_request(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|ws| async move {
        let (sink, stream) = ws.split();
        stream
            .try_filter_map(|mut msg| async {
                let result = if let Message::Binary(data) = &mut msg {
                    println!("received {} bytes, compressing..", data.len());
                    let mut compressed = Vec::new();
                    let mut gz = GzEncoder::new(&data[..], Default::default());
                    gz.read_to_end(&mut compressed).map_err(axum::Error::new)?;
                    *data = compressed.into();
                    println!("compressed into {} bytes, sending..", data.len());
                    Some(msg)
                } else {
                    None
                };
                Ok(result)
            })
            .forward(sink)
            .await
            .unwrap();
    })
}
