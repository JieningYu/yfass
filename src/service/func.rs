use std::borrow::Cow;

use axum::{Json, body::Body, extract::Path};
use futures_util::TryStreamExt as _;
use serde::{Deserialize, Serialize};
use yfass::{func, user};

use crate::{Auth, ContentType, Error, PermissionFlags, State};

fn validate_key_param(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::InvalidKeyFormat);
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        .then_some(())
        .ok_or(Error::InvalidKeyFormat)
}

const PERMISSION_UPLOAD: u32 = PermissionFlags::WRITE.bits();
pub const PATH_UPLOAD: &str = "/api/upload/{key}";

/// Deploys a function by uploading a tarball.
///
/// # Request
///
/// - Authentication is required with permission `WRITE`.
/// - Body is required to receive a tarball or gzipped tarball.
pub async fn upload(
    cx: State,
    Auth(token): Auth<PERMISSION_UPLOAD>,
    ContentType(ty): ContentType,
    Path(key): Path<func::OwnedKey>,
    body: Body,
) -> Result<(), Error> {
    validate_key_param(&key.name)?;
    validate_key_param(&key.version)?;

    let user = cx.users.user_name(&token).ok_or(Error::Unauthorized)?;

    const CONTENT_TYPE_TAR: &str = "application/x-tar";
    const CONTENT_TYPE_GZIP: &str = "application/gzip";
    const CONTENT_TYPE_GZIP_NON_STANDARD: &str = "application/x-gzip";

    let group = Some(user::Group::Singular(user));
    let reader =
        tokio_util::io::StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));

    match &*ty {
        // .tar file
        CONTENT_TYPE_TAR => {
            cx.funcs
                .add_func(key.as_ref(), group, &mut tokio_tar::Archive::new(reader))
                .await?;
        }
        // .tar.gz / .tgz file
        CONTENT_TYPE_GZIP | CONTENT_TYPE_GZIP_NON_STANDARD => {
            cx.funcs
                .add_func(
                    key.as_ref(),
                    group,
                    &mut tokio_tar::Archive::new(
                        async_compression::tokio::bufread::GzipDecoder::new(reader),
                    ),
                )
                .await?
        }
        _ => return Err(Error::UnsupportedArchiveType),
    }

    Ok(())
}

const PERMISSION_GET: u32 = PermissionFlags::READ.bits();
pub const PATH_GET: &str = "/api/get/{key}";

/// Retrives a function information.
///
/// # Request
///
/// - Authentication is required with permission `READ`.
///
/// # Response
///
/// - Responsed with json body [`func::Function`].
pub async fn get(
    cx: State,
    Auth(_): Auth<PERMISSION_GET>,
    Path(key): Path<func::OwnedKey>,
) -> Result<Json<func::Function>, Error> {
    cx.funcs
        .get(key.as_ref())
        .map(|f| f.read().clone())
        .ok_or(Error::NotFound)
        .map(Json)
}

const PERMISSION_OVERRIDE_CONFIG: u32 = PermissionFlags::WRITE.bits();
pub const PATH_OVERRIDE_CONFIG: &str = "/api/override/{key}";

/// Overrides configuration of a function.
///
/// # Request
///
/// - Authentication is required with permission `WRITE` and _the group requirement by the function._
/// - Request body is JSON format of [`func::Config`].
pub async fn override_config(
    cx: State,
    Auth(token): Auth<PERMISSION_OVERRIDE_CONFIG>,
    Path(key): Path<func::OwnedKey>,
    Json(config): Json<func::Config>,
) -> Result<(), Error> {
    let func = cx.funcs.get(key.as_ref()).ok_or(Error::NotFound)?;
    cx.users
        .auth(&token, func.read().config.group.iter().map(Cow::Borrowed))
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.funcs.modify_config(key.as_ref(), config)?;
    Ok(())
}

#[derive(Deserialize)]
pub struct AliasRequest {
    /// `Some` for alias addition or modification;
    /// `None` for removals.
    pub alias: Option<String>,
}

const PERMISSION_ALIAS: u32 = PermissionFlags::WRITE.bits();
pub const PATH_ALIAS: &str = "/api/alias/{key}";

/// Overrides alias of a function.
///
/// # Request
///
/// - Authentication is required with permission `WRITE` and _the group requirement by the function._
/// - Request body is JSON format of [`AliasRequest`].
pub async fn alias(
    cx: State,
    Auth(token): Auth<PERMISSION_ALIAS>,
    Path(key): Path<func::OwnedKey>,
    Json(AliasRequest { alias }): Json<AliasRequest>,
) -> Result<(), Error> {
    if let Some(alias) = &alias {
        validate_key_param(alias)?;
    }

    let func = cx.funcs.get(key.as_ref()).ok_or(Error::NotFound)?;
    cx.users
        .auth(&token, func.read().config.group.iter().map(Cow::Borrowed))
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.funcs.modify_alias(key.as_ref(), alias)?;
    Ok(())
}

const PERMISSION_REMOVE: u32 = PermissionFlags::REMOVE.bits();
pub const PATH_REMOVE: &str = "/api/remove/{key}";

/// Removes a function.
///
/// # Request
///
/// - Authentication is required with permission `REMOVE` and _the group requirement by the function._
pub async fn remove(
    cx: State,
    Auth(token): Auth<PERMISSION_REMOVE>,
    Path(key): Path<func::OwnedKey>,
) -> Result<(), Error> {
    let func = cx.funcs.get(key.as_ref()).ok_or(Error::NotFound)?;
    cx.users
        .auth(&token, func.read().config.group.iter().map(Cow::Borrowed))
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.funcs.remove_func(key.as_ref())?;
    Ok(())
}

const PERMISSION_DEPLOY: u32 = PermissionFlags::EXECUTE.bits();
pub const PATH_DEPLOY: &str = "/api/deploy/{key}";

/// Deploys (or start) a function.
///
/// # Request
///
/// - Authentication is required with permission `EXECUTE` and _the group requirement by the function._
pub async fn deploy(
    cx: State,
    Auth(token): Auth<PERMISSION_DEPLOY>,
    Path(key): Path<func::OwnedKey>,
) -> Result<(), Error> {
    let func = cx.funcs.get(key.as_ref()).ok_or(Error::NotFound)?;
    cx.users
        .auth(&token, func.read().config.group.iter().map(Cow::Borrowed))
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.start_fn(key.as_ref()).await
}

const PERMISSION_KILL: u32 = PermissionFlags::EXECUTE.bits();
pub const PATH_KILL: &str = "/api/kill/{key}";

/// Kills (or stop) a function.
///
/// # Request
///
/// - Authentication is required with permission `EXECUTE` and _the group requirement by the function._
pub async fn kill(
    cx: State,
    Auth(token): Auth<PERMISSION_KILL>,
    Path(key): Path<func::OwnedKey>,
) -> Result<(), Error> {
    let func = cx.funcs.get(key.as_ref()).ok_or(Error::NotFound)?;
    cx.users
        .auth(&token, func.read().config.group.iter().map(Cow::Borrowed))
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.stop_fn(key.as_ref()).await
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub running: bool,
}

const PERMISSION_STATUS: u32 = PermissionFlags::READ.bits();
pub const PATH_STATUS: &str = "/api/status/{key}";

pub async fn status(
    cx: State,
    Auth(_): Auth<PERMISSION_STATUS>,
    Path(key): Path<func::OwnedKey>,
) -> Result<Json<StatusResponse>, Error> {
    let running = cx.is_running(key.as_ref());
    Ok(Json(StatusResponse { running }))
}
