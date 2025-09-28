//! FASS platform implementation.

use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use axum::{
    Router, ServiceExt as _,
    body::Body,
    http::{self, StatusCode},
    middleware,
    response::IntoResponse,
};
use bitflags::bitflags;
use clap::Parser as _;
use hyper_util::client;
use parking_lot::Mutex;
use rand::{SeedableRng as _, rngs::StdRng};
use serde::Serialize;
use tokio_tungstenite::tungstenite;
use tower_layer::Layer as _;
use tracing_subscriber::EnvFilter;
use yfass::{
    func::{self, FunctionManager, OwnedKey},
    os,
    sandbox::{self, Sandbox},
    user::{self, Permission, UserManager},
};

mod proxy;
mod service;

#[derive(Debug)]
struct LocalCx {
    funcs: FunctionManager,
    proxies: scc::HashIndex<String, http::uri::Authority>,
    users: UserManager,

    sandbox: os::SandboxImpl,
    handles: scc::HashMap<OwnedKey, os::SandboxHandleImpl>,

    client: client::legacy::Client<client::legacy::connect::HttpConnector, Body>,
    host_with_dot_prefixed: String,
    host_port_with_dot_prefixed: String,

    rng: Mutex<StdRng>,
}

fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_level(true)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    rt.block_on(main_async())
}

async fn main_async() {
    let args = Args::parse();
    let addr = SocketAddr::new(
        args.addr
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        args.port,
    );
    let root_dir = args.path.unwrap_or_else(|| PathBuf::from("./"));
    let host = args.host;

    let mut rng = StdRng::from_os_rng();

    let client = client::legacy::Builder::new(hyper_util::rt::TokioExecutor::new())
        .http1_ignore_invalid_headers_in_responses(true)
        .http1_preserve_header_case(true)
        .set_host(false)
        .build(client::legacy::connect::HttpConnector::new());

    let cx = Arc::new(LocalCx {
        funcs: FunctionManager::new(&root_dir),
        users: UserManager::new(&mut rng, &root_dir),
        proxies: scc::HashIndex::new(),
        handles: scc::HashMap::new(),
        sandbox: os::SandboxImpl::default(),
        rng: Mutex::new(rng),
        client,
        host_with_dot_prefixed: format!(".{}", host),
        host_port_with_dot_prefixed: format!(".{}:{}", host, args.port),
    });

    cx.funcs
        .read_from_fs()
        .expect("failed to read functions from fs");
    cx.users
        .read_from_fs()
        .expect("failed to read users from fs");

    let router = Router::new()
        // func services
        .route(
            service::func::PATH_UPLOAD,
            axum::routing::post(service::func::upload),
        )
        .route(
            service::func::PATH_GET,
            axum::routing::get(service::func::get),
        )
        .route(
            service::func::PATH_OVERRIDE_CONFIG,
            axum::routing::put(service::func::override_config),
        )
        .route(
            service::func::PATH_ALIAS,
            axum::routing::patch(service::func::alias),
        )
        .route(
            service::func::PATH_REMOVE,
            axum::routing::delete(service::func::remove),
        )
        .route(
            service::func::PATH_DEPLOY,
            axum::routing::post(service::func::deploy),
        )
        .route(
            service::func::PATH_KILL,
            axum::routing::post(service::func::kill),
        )
        .route(
            service::func::PATH_STATUS,
            axum::routing::get(service::func::status),
        )
        // user services
        .route(
            service::user::PATH_ADD,
            axum::routing::post(service::user::add),
        )
        .route(
            service::user::PATH_GET,
            axum::routing::get(service::user::get),
        )
        .route(
            service::user::PATH_REMOVE,
            axum::routing::delete(service::user::remove),
        )
        .route(
            service::user::PATH_REQUEST_TOKEN,
            axum::routing::post(service::user::request_token),
        )
        .route(
            service::user::PATH_MODIFY,
            axum::routing::put(service::user::modify),
        )
        // layers being executed from bottom to top in axum's ordering
        .route_layer(tower_http::trace::TraceLayer::new_for_http())
        // somehow one found <()> looks like F35 engine from outside
        .with_state::<()>(cx.clone());

    tokio::spawn({
        let cloned_cx = cx.clone();
        async move {
            const WRITE_DURATION: tokio::time::Duration = tokio::time::Duration::from_mins(12);
            let cx = cloned_cx;
            loop {
                tokio::time::sleep(WRITE_DURATION).await;
                save_data(&cx).await;
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(
        listener,
        middleware::from_fn_with_state(cx.clone(), proxy::forward_http_req)
            .layer(router)
            .into_make_service(),
    )
    .with_graceful_shutdown(async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }

        save_data(&cx).await
    })
    .await
    .unwrap();
    tracing::info!("server stopped");
}

impl LocalCx {
    async fn start_fn(&self, key: func::Key<'_>) -> Result<(), Error> {
        let func = self.funcs.get(key).ok_or(Error::NotFound)?;

        let config;
        let auth_uri;

        {
            let rg = func.read();
            // need to clone it or non-async read lock will cause deadlock across await points
            config = rg.config.sandbox.clone();
            auth_uri = http::uri::Authority::from_maybe_shared(rg.config.addr.to_string())?;
        }

        let handle = Sandbox::spawn(&self.sandbox, &config, &self.funcs.contents_path(key)).await?;

        if let Err((_, handle)) = self.handles.insert_sync(key.into_owned(), handle) {
            sandbox::Handle::kill(handle).await;
            Err(Error::InstanceAlreadyRunning)
        } else {
            drop(self.proxies.insert_sync(key.to_host_prefix(), auth_uri));
            Ok(())
        }
    }

    async fn stop_fn(&self, key: func::Key<'_>) -> Result<(), Error> {
        let (_, handle) = self.handles.remove_sync(&key).ok_or(Error::NotFound)?;
        sandbox::Handle::kill(handle).await;
        self.proxies.remove_sync(&key.to_host_prefix());
        Ok(())
    }

    fn is_running(&self, key: func::Key<'_>) -> bool {
        self.handles
            .read_sync(&key, |_, handle| sandbox::Handle::is_running(handle))
            .unwrap_or_default()
    }
}

type State = axum::extract::State<Arc<LocalCx>>;

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct PermissionFlags: u32 {
        const READ    = 1 << 0;
        const WRITE   = 1 << 1;
        const EXECUTE = 1 << 2;
        const REMOVE  = 1 << 3;
        const ADMIN   = 1 << 4;
        const ROOT    = 1 << 5;
    }
}

impl PermissionFlags {
    fn to_permission(self) -> Option<Permission> {
        Some(match self {
            Self::READ => Permission::Read,
            Self::WRITE => Permission::Write,
            Self::EXECUTE => Permission::Execute,
            Self::REMOVE => Permission::Remove,
            Self::ADMIN => Permission::Admin,
            Self::ROOT => Permission::Root,
            _ => return None,
        })
    }
}

const AUTH_PREFIX: &str = "Bearer ";

struct Auth<const P: u32>(String);

impl<const P: u32> axum::extract::FromRequestParts<Arc<LocalCx>> for Auth<P> {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &Arc<LocalCx>,
    ) -> Result<Self, Self::Rejection> {
        let flags = PermissionFlags::from_bits_retain(P);
        let header = parts
            .headers
            .remove(http::header::AUTHORIZATION)
            .ok_or(Error::Unauthorized)?;

        let token = header
            .to_str()?
            .strip_prefix(AUTH_PREFIX)
            .ok_or(Error::InvalidAuthMethod)?
            .trim();

        if state.users.auth(
            token,
            flags
                .iter()
                .filter_map(PermissionFlags::to_permission)
                .map(user::Group::Permission)
                .map(Cow::Owned),
        ) {
            Ok(Self(token.to_owned()))
        } else {
            Err(Error::PermissionDenied)
        }
    }
}

struct ContentType(String);

impl<S: Sync> axum::extract::FromRequestParts<S> for ContentType {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .remove(http::header::CONTENT_TYPE)
            .ok_or(Error::MissingContentType)?;
        Ok(Self(header.to_str()?.to_owned()))
    }
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
enum Error {
    #[error("unauthorized account")]
    Unauthorized,
    #[error("permission denied")]
    PermissionDenied,
    #[error("invalid header value: {0}")]
    InvalidHeaderEncoding(#[from] http::header::ToStrError),
    #[error("invalid authentication method, only bearer authentication is supported.")]
    InvalidAuthMethod,
    #[error("function manager error: {0}")]
    FunctionManager(#[from] func::ManagerError),
    #[error("user manager error: {0}")]
    UserManager(#[from] user::ManagerError),
    #[error("missing content-type header")]
    MissingContentType,
    #[error(
        "unsupported archive type, the only supported archive type is tarball with optional gzip compression"
    )]
    UnsupportedArchiveType,
    #[error("specified resource not found")]
    NotFound,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid key format. the permitted key characters are: a-z, 0-9, -")]
    InvalidKeyFormat,
    #[error("another instance of this function is already running")]
    InstanceAlreadyRunning,
    #[error("invalid uri parsed from socket address: {0}")]
    InvalidSocketAddrAsUri(#[from] http::uri::InvalidUri),
    #[error("invalid username format. the permitted key characters are: A-Z, a-z, 0-9, -")]
    InvalidUsernameFormat,
    #[error("attempt to modify information of root user")]
    ModifyRootUser,
    #[error("the function you are trying to access is not running or it does not exist")]
    FunctionNotRunning,
    #[error("missing HOST header or it is invalid")]
    MissingHost,
    #[error("invalid uri parts from host: {0}")]
    InvalidUriParts(#[from] http::uri::InvalidUriParts),
    #[error("HTTP client error occurred: {0}")]
    Client(#[from] client::legacy::Error),
    #[error("websocket connection error occurred: {0}")]
    WebsocketConnection(#[from] tungstenite::Error),
}

impl Error {
    #[inline]
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized | Self::InvalidAuthMethod => StatusCode::UNAUTHORIZED,

            Self::PermissionDenied
            | Self::InvalidKeyFormat
            | Self::InvalidUsernameFormat
            | Self::ModifyRootUser
            | Self::FunctionNotRunning => StatusCode::FORBIDDEN,

            Self::InvalidHeaderEncoding(_)
            | Self::MissingContentType
            | Self::UnsupportedArchiveType
            | Self::MissingHost
            | Self::InvalidUriParts(_) => StatusCode::BAD_REQUEST,

            Self::NotFound => StatusCode::NOT_FOUND,

            Self::Io(_)
            | Self::InvalidSocketAddrAsUri(_)
            | Self::Client(_)
            | Self::WebsocketConnection(_) => StatusCode::INTERNAL_SERVER_ERROR,

            Self::InstanceAlreadyRunning => StatusCode::CONFLICT,

            // function manager
            Self::FunctionManager(e) => match e {
                func::ManagerError::NotAliased => StatusCode::FORBIDDEN,
                func::ManagerError::Io(_)
                | func::ManagerError::ParseJson(_)
                | func::ManagerError::Initialized => StatusCode::INTERNAL_SERVER_ERROR,
                func::ManagerError::Duplicated => StatusCode::CONFLICT,
                func::ManagerError::NotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::IM_A_TEAPOT, // non-exhaustive aftermath
            },

            // user manager
            Self::UserManager(e) => match e {
                user::ManagerError::Io(_)
                | user::ManagerError::ParseJson(_)
                | user::ManagerError::Initialized => StatusCode::INTERNAL_SERVER_ERROR,
                user::ManagerError::Duplicated => StatusCode::CONFLICT,
                user::ManagerError::NotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::IM_A_TEAPOT, // non-exhaustive aftermath
            },
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        #[derive(Serialize)]
        struct Serialized {
            error: String,
        }

        (
            self.status_code(),
            axum::Json(Serialized {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Debug, clap::Parser)]
struct Args {
    /// Path to the root directory of the server.
    #[arg(short, long)]
    path: Option<PathBuf>,
    /// IP address host (without port number) to bind to.
    #[arg(short, long)]
    addr: Option<IpAddr>,
    /// Port to bind to.
    #[arg(short, long, default_value_t = 8080)]
    port: u16,
    /// Host name to use.
    #[arg(short, long)]
    host: String,
}

async fn save_data(cx: &LocalCx) {
    let span = tracing::info_span!("writing data into filesystem");
    let mut e = None;

    if cx.funcs.is_dirty() {
        e = Some(e.unwrap_or_else(|| span.enter()));
        drop(cx.funcs.write_all_to_fs().await.inspect_err(|err| {
            tracing::error!("failed to write function information into filesystem: {err}")
        }))
    }

    if cx.users.is_dirty() {
        e = Some(e.unwrap_or_else(|| span.enter()));
        drop(cx.users.write_all_to_fs().await.inspect_err(|err| {
            tracing::error!("failed to write user information into filesystem: {err}")
        }))
    }

    drop(e); // emit unread warnings
}
