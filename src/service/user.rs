use std::borrow::Cow;

use axum::{Json, extract::Path};
use serde::{Deserialize, Serialize};
use time::Duration;
use yfass::user::{self, User};

use crate::{Auth, Error, PermissionFlags, State};

fn validate_username_param(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::InvalidUsernameFormat);
    }
    name.chars()
        .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '-')
        .then_some(())
        .ok_or(Error::InvalidKeyFormat)
}

#[derive(Serialize, Deserialize)]
pub struct ClientUser {
    pub name: String,
    #[serde(default)]
    pub groups: Box<[user::Group]>,
}

fn client_from_ref(user: &User) -> ClientUser {
    ClientUser {
        name: user.name.clone(),
        groups: user.groups.iter().cloned().collect(),
    }
}

const ADD_PERMISSION: u32 = PermissionFlags::ADMIN.bits();
pub(crate) const PATH_ADD: &str = "/api/user/add";

/// Adds a user.
///
/// # Request
///
/// - Authentication is required with permission `ADMIN`.
/// - Request body is JSON format of [`ClientUser`].
pub async fn add(
    cx: State,
    Auth(token): Auth<ADD_PERMISSION>,
    Json(req): Json<ClientUser>,
) -> Result<(), Error> {
    validate_username_param(&req.name)?;

    cx.users
        .auth(
            &token,
            req.groups.iter().filter_map(|g| {
                if let user::Group::Permission(_) = g {
                    Some(Cow::Borrowed(g))
                } else {
                    None
                }
            }),
        )
        .then_some(())
        .ok_or(Error::PermissionDenied)?;

    let user = User::new(req.name.to_ascii_lowercase(), req.groups.into_iter());
    cx.users.add(user)?;
    Ok(())
}

const REMOVE_PERMISSION: u32 = PermissionFlags::ROOT.bits();
pub(crate) const PATH_REMOVE: &str = "/api/user/remove/{user}";

/// Removes a user.
///
/// # Request
///
/// - Authentication is required with permission `ROOT`.
pub async fn remove(
    cx: State,
    Auth(_): Auth<REMOVE_PERMISSION>,
    Path(name): Path<String>,
) -> Result<(), Error> {
    cx.users.remove(&name).map_err(Into::into)
}

const GET_PERMISSION: u32 = PermissionFlags::empty().bits();
pub(crate) const PATH_GET: &str = "/api/user/get/{user}";

/// Gets information of a user.
///
/// # Request
///
/// - Authentication is required with permission `ADMIN` for checking **other users.**
///
/// # Response
///
/// The response body is the JSON form of [`ClientUser`].
pub async fn get(
    cx: State,
    Auth(token): Auth<GET_PERMISSION>,
    name_optional: Option<Path<String>>,
) -> Result<Json<ClientUser>, Error> {
    let root = ClientUser {
        name: "root".to_owned(),
        groups: Box::new([user::Group::Permission(user::Permission::Root)]),
    };

    let val = cx.users.peek_from_token(&token, |this| {
        (
            name_optional
                .as_ref()
                .is_none_or(|Path(name)| name == &this.name)
                .then(|| client_from_ref(this)),
            this.is_in(&user::Group::Permission(user::Permission::Admin)),
        )
    })?;

    match val {
        // non-root, getting self
        Some((Some(this), _)) => Ok(this),
        // non-root, getting others with admin perms
        Some((_, true)) => {
            // in this case name_optional is definately non-empty
            cx.users
                .peek(&name_optional.as_ref().unwrap().0, client_from_ref)
                .map_err(Into::into)
                .map(|o| o.unwrap_or(root))
        }
        // non-root but without corresponding perms
        Some((_, false)) => Err(Error::PermissionDenied),
        // root
        None => name_optional
            .filter(|Path(n)| n != "root")
            .map_or(Ok(root), |Path(n)| {
                cx.users
                    .peek(&n, client_from_ref)
                    .map_err(Into::into)
                    // guaranteed not to be root as previously filtered out. safe to unwrap
                    .map(Option::unwrap)
            }),
    }
    .map(Json)
}

#[inline]
const fn default_token_duration_days() -> u32 {
    10
}

#[derive(Deserialize)]
pub struct RequestTokenRequest {
    /// Token valid duration in **days.**
    #[serde(default = "default_token_duration_days")]
    pub duration: u32,
    /// Username of the account whose token is being allocated.
    pub user: String,
}

const REQUEST_TOKEN_PERMISSION: u32 = PermissionFlags::ADMIN.bits();
pub(crate) const PATH_REQUEST_TOKEN: &str = "/api/user/request-token";

/// Requests a new token for the specified user.
///
/// # Request
///
/// - Authentication is required with permission `ADMIN` for checking **all users.**
/// - Request body is JSON form of [`RequestTokenRequest`].
///
/// # Response
///
/// The response body is a text literal directly containing the token.
pub async fn request_token(
    cx: State,
    Auth(_): Auth<REQUEST_TOKEN_PERMISSION>,
    Json(req): Json<RequestTokenRequest>,
) -> Result<String, Error> {
    cx.users
        .add_token(
            &req.user,
            &mut *cx.rng.lock(),
            Duration::days(req.duration as i64),
        )
        .map_err(Into::into)
}

const MODIFY_PERMISSION: u32 = PermissionFlags::ADMIN.bits();
pub(crate) const PATH_MODIFY: &str = "/api/user/modify";

/// Modifies information (currently only group is supported) of a user.
///
/// # Request
///
/// - Authentication is required with permission `ADMIN` for checking **all users.**
/// - Request body is JSON form of [`ClientUser`].
pub async fn modify(
    cx: State,
    Auth(token): Auth<MODIFY_PERMISSION>,
    Json(user): Json<ClientUser>,
) -> Result<(), Error> {
    cx.users
        .auth(
            &token,
            user.groups.iter().filter_map(|g| {
                if let user::Group::Permission(_) = g {
                    Some(Cow::Borrowed(g))
                } else {
                    None
                }
            }),
        )
        .then_some(())
        .ok_or(Error::PermissionDenied)?;
    cx.users
        .peek_mut(&user.name, |u| {
            u.groups = user.groups.into_iter().collect();
        })?
        .ok_or(Error::ModifyRootUser)
}
