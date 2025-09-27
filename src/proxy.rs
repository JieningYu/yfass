use axum::{
    body::Body,
    extract::Request,
    http::{self, Uri},
    response::Response,
};

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
        .and_then(|s| s.strip_suffix(&cx.host_with_dot_prefixed))
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
    *request.uri_mut() = Uri::from_parts(uri_parts)?;

    cx.client
        .request(request)
        .await
        .map(|r| r.map(Body::new))
        .map_err(Into::into)
}
