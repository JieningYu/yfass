use axum::{
    body::{Body, Bytes},
    extract::{FromRequestParts as _, Request},
    http::{self, Uri, uri::Scheme},
    response::Response,
};
use futures_util::{SinkExt as _, StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use tokio_tungstenite::tungstenite;

use crate::{Error, State};

/// Forwards HTTP requests to functions.
pub async fn forward_http_req(
    cx: State,
    mut request: Request,
    next: axum::middleware::Next,
) -> Result<Response, Error> {
    let Some(func_key) = request
        .headers()
        .get(http::header::HOST)
        .ok_or(Error::MissingHost)?
        .to_str()
        .ok()
        // .inspect(|host| tracing::debug!("proxy: received request to hostname {host}"))
        .and_then(|s| {
            s.strip_suffix(&cx.host_with_dot_prefixed)
                .or_else(|| s.strip_suffix(&cx.host_port_with_dot_prefixed))
        })
    else {
        // cant strip with dot prefixed host. not a subdomain tho
        return Ok(next.run(request).await);
    };

    let authority = cx
        .proxies
        .peek_with(func_key, |_, a| a.clone())
        .ok_or(Error::FunctionNotRunning)?;

    let mut uri_parts = std::mem::take(request.uri_mut()).into_parts();
    uri_parts.authority = Some(authority);
    uri_parts.scheme = Some(Scheme::HTTP);
    *request.uri_mut() = Uri::from_parts(uri_parts)?;

    tracing::debug!(
        "proxy: forwarding request to function with uri {}",
        request.uri()
    );

    // forward websocket requests
    if maybe_ws_request(&request) {
        let mut parts;
        request = {
            let (p, body) = request.into_parts();
            // clone it for use of websocket extractor as it takes mutable parts and we don't want to
            // modify the one sent to function.
            // while this is costly but it only occurs once per websocket connection.
            parts = p.clone();
            Request::from_parts(p, body)
        };

        let mut uri_parts = std::mem::take(request.uri_mut()).into_parts();
        uri_parts.scheme = Some("ws".try_into().unwrap());
        *request.uri_mut() = Uri::from_parts(uri_parts)?;

        if let Ok(upgrade) =
            axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &()).await
        {
            tracing::debug!("proxy: forwarding websocket upgrade request");

            // elide the request body as it should be empty
            let request = Request::from_parts(request.into_parts().0, ());
            let (stream, _resp) = tokio_tungstenite::connect_async(request).await?;
            let resp = upgrade.on_upgrade(|ws| async {
                let (s2c_sink, c2s_stream) = ws.split();
                let (s2f_sink, f2s_stream) = stream.split();

                // client -> server -> function
                tokio::spawn(
                    c2s_stream
                        .map_ok(msg_ts_from_axum)
                        .forward(s2f_sink.sink_map_err(axum::Error::new))
                        .inspect_err(|err| tracing::warn!("websocket error from connection chain client -> server -> function: {err}")),
                );

                // function -> server -> client
                tokio::spawn(
                    f2s_stream
                        .try_filter_map(|o| std::future::ready(Ok(msg_axum_from_ts(o))))
                        .map_err(axum::Error::new)
                        .forward(s2c_sink)
                        .inspect_err(|err| tracing::warn!("websocket error from connection chain function -> server -> client: {err}"))
                );
            });

            return Ok(resp);
        }
        // else: this is not a websocket request
    }

    cx.client
        .request(request)
        .await
        .map(|r| r.map(Body::new))
        .map_err(Into::into)
}

fn maybe_ws_request(request: &Request) -> bool {
    if request.version() <= http::Version::HTTP_11 {
        header_contains(request.headers(), http::header::CONNECTION, "upgrade")
            && header_eq(request.headers(), http::header::UPGRADE, "websocket")
    } else {
        request.method() == http::Method::CONNECT
    }
}

fn utf8_bytes_axum_from_ts(msg: tungstenite::Utf8Bytes) -> axum::extract::ws::Utf8Bytes {
    //SAFETY: ts' type already guarantees utf8 validity. we have to cancel the check for performance
    // cant find a better way to do this. tol
    unsafe { String::from_utf8_unchecked(Bytes::from(msg).into()).into() }
}

fn utf8_bytes_ts_from_axum(msg: axum::extract::ws::Utf8Bytes) -> tungstenite::Utf8Bytes {
    //SAFETY: axum's type already guarantees utf8 validity.
    unsafe { tungstenite::Utf8Bytes::from_bytes_unchecked(msg.into()) }
}

// helper functions from axum

#[inline]
fn header_eq(headers: &http::HeaderMap, key: http::HeaderName, value: &'static str) -> bool {
    if let Some(header) = headers.get(&key) {
        header.as_bytes().eq_ignore_ascii_case(value.as_bytes())
    } else {
        false
    }
}

#[inline]
fn header_contains(headers: &http::HeaderMap, key: http::HeaderName, value: &'static str) -> bool {
    let header = if let Some(header) = headers.get(&key) {
        header
    } else {
        return false;
    };

    if let Ok(header) = std::str::from_utf8(header.as_bytes()) {
        header.to_ascii_lowercase().contains(value)
    } else {
        false
    }
}

fn msg_axum_from_ts(message: tungstenite::Message) -> Option<axum::extract::ws::Message> {
    use tokio_tungstenite::tungstenite as ts;
    match message {
        ts::Message::Text(text) => Some(axum::extract::ws::Message::Text(utf8_bytes_axum_from_ts(
            text,
        ))),
        ts::Message::Binary(binary) => Some(axum::extract::ws::Message::Binary(binary)),
        ts::Message::Ping(ping) => Some(axum::extract::ws::Message::Ping(ping)),
        ts::Message::Pong(pong) => Some(axum::extract::ws::Message::Pong(pong)),
        ts::Message::Close(Some(close)) => Some(axum::extract::ws::Message::Close(Some(
            axum::extract::ws::CloseFrame {
                code: close.code.into(),
                // copies the slice internally as we don't have the access to private constructor.
                // but frame closing is not the hot spot anyway.
                reason: utf8_bytes_axum_from_ts(close.reason),
            },
        ))),
        ts::Message::Close(None) => Some(axum::extract::ws::Message::Close(None)),
        // we can ignore `Frame` frames as recommended by the tungstenite maintainers
        // https://github.com/snapview/tungstenite-rs/issues/268
        ts::Message::Frame(_) => None,
    }
}

fn msg_ts_from_axum(message: axum::extract::ws::Message) -> tungstenite::Message {
    use tokio_tungstenite::tungstenite as ts;
    match message {
        axum::extract::ws::Message::Text(text) => ts::Message::Text(utf8_bytes_ts_from_axum(text)),
        axum::extract::ws::Message::Binary(binary) => ts::Message::Binary(binary),
        axum::extract::ws::Message::Ping(ping) => ts::Message::Ping(ping),
        axum::extract::ws::Message::Pong(pong) => ts::Message::Pong(pong),
        axum::extract::ws::Message::Close(Some(close)) => {
            ts::Message::Close(Some(ts::protocol::CloseFrame {
                code: ts::protocol::frame::coding::CloseCode::from(close.code),
                reason: utf8_bytes_ts_from_axum(close.reason),
            }))
        }
        axum::extract::ws::Message::Close(None) => ts::Message::Close(None),
    }
}
