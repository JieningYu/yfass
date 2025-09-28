//! User system for managing the platform.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt::Display,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
};

use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use time::{Duration, UtcDateTime};

/// User of the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Name of the user.
    ///
    /// This should be immutable.
    pub name: String,
    /// Groups of the user.
    ///
    /// Do not check using the set directly; Instead, use [`Self::is_in`] to check whether a user is in a group.
    pub groups: HashSet<Group>,

    tokens: HashMap<String, UtcDateTime>, // token ->  expiration instant
}

impl User {
    /// Creates a new user.
    pub fn new<I>(name: String, groups: I) -> Self
    where
        I: IntoIterator<Item = Group>,
    {
        Self {
            name,
            groups: groups.into_iter().collect(),
            tokens: HashMap::new(),
        }
    }

    /// Checks whether this user is in the specified group.
    #[inline]
    pub fn is_in(&self, group: &Group) -> bool {
        match group {
            Group::Singular(name) => &self.name == name,
            _ => self.groups.contains(group),
        }
    }

    /// Checks whether this user has a specified token and is not expired.
    #[inline]
    pub fn is_token_valid(&self, token: &str) -> bool {
        self.tokens
            .get(token)
            .is_some_and(|time| UtcDateTime::now() < *time)
    }

    fn add_token<R>(&mut self, rng: R, duration: Duration) -> String
    where
        R: RngCore,
    {
        // remove expired tokens. we got mutable access why not do this
        self.tokens.retain(|_, time| UtcDateTime::now() < *time);

        let token = gen_token(rng);
        self.tokens
            .insert(token.clone(), UtcDateTime::now() + duration);
        token
    }

    /// Clears all tokens of this user.
    pub fn clear_tokens(&mut self) {
        self.tokens.clear();
    }
}

/// Generates a random token from given [`RngCore`].
pub fn gen_token<R>(mut rng: R) -> String
where
    R: RngCore,
{
    const LEN_TOKEN: usize = 32;

    let mut token_raw = [0u8; LEN_TOKEN];
    rng.fill_bytes(&mut token_raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_raw)
}

/// Group of a user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Group {
    /// Group specifying a permission.
    Permission(Permission),
    /// Group specifying a specified user.
    Singular(String),
    /// Custom group category.
    Custom(String),
}

const UG_KEY_SINGULAR: &str = "singular";
const UG_KEY_PERMISSION: &str = "permission";
const UG_KEY_CUSTOM: &str = "custom";

/// Permission of a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read permission to function information.
    Read,
    /// Permission to upload new functions and modify information of existing functions.
    Write,
    /// Permission to execute functions.
    Execute,
    /// Permission to delete functions.
    Remove,
    /// Permission to manage accounts.
    Admin,
    /// Root privilege.
    Root,
}

impl Permission {
    /// Checks whether this permission contains the other permission.
    pub const fn contains(self, other: Self) -> bool {
        if matches!(self, Self::Root) {
            return true;
        }

        match other {
            Permission::Read => matches!(
                self,
                Permission::Read | Permission::Write | Permission::Remove | Permission::Admin
            ),
            Permission::Write => matches!(self, Permission::Write | Permission::Admin),
            Permission::Remove => matches!(self, Permission::Remove | Permission::Admin),
            Permission::Admin => matches!(self, Permission::Admin),
            Permission::Execute => matches!(self, Permission::Execute | Permission::Admin),
            Permission::Root => false,
        }
    }
}

impl Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Group::Permission(permission) => {
                write!(f, "{UG_KEY_PERMISSION}:")?;
                permission.serialize(f)
            }
            Group::Singular(user) => write!(f, "{UG_KEY_SINGULAR}:{user}"),
            Group::Custom(group) => write!(f, "{UG_KEY_CUSTOM}:{group}"),
        }
    }
}

impl FromStr for Group {
    type Err = ParseGroupError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (key, value) = s.split_once(':').ok_or(ParseGroupError::MissingKey)?;
        match key {
            UG_KEY_PERMISSION => Permission::deserialize(serde::de::value::StrDeserializer::<
                '_,
                serde::de::value::Error,
            >::new(value))
            .map(Self::Permission)
            .map_err(|err| ParseGroupError::InvalidPermission(value.to_owned(), err)),
            UG_KEY_CUSTOM => Ok(Self::Custom(value.to_owned())),
            UG_KEY_SINGULAR => Ok(Self::Singular(value.to_owned())),
            _ => Err(ParseGroupError::MissingKey),
        }
    }
}

impl Serialize for Group {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Group {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = Group;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a group")
            }

            #[inline]
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Group::from_str(v).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// Error when parsing a [`Group`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ParseGroupError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("invalid permission: {0}, error: {1}")]
    InvalidPermission(String, serde::de::value::Error),
    #[error("missing key")]
    MissingKey,
}

/// Manager of users.
#[derive(Debug)]
pub struct UserManager {
    users: scc::HashMap<String, User>,      // user name -> user
    tokens: scc::HashIndex<String, String>, // token -> user name
    root_dir: Arc<Path>,

    root_token: String,

    dirty: AtomicBool,
}

const ROOT_USERNAME: &str = "root";

#[derive(Serialize, Deserialize)]
struct SerializedUsers {
    users: Box<[User]>,
}

const USERS_FILE: &str = "users.json";

impl UserManager {
    fn mark_dirty(&self) {
        self.dirty.store(true, atomic::Ordering::Relaxed);
    }

    /// Checks whether the user manager is dirty and needs to be written to the filesystem.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(atomic::Ordering::Relaxed)
    }

    /// Creates an empty, uninitialized user manager.
    ///
    /// For loading users from the filesystem, use [`Self::read_from_fs`].
    pub fn new<P, R>(rng: R, root_dir: P) -> Self
    where
        P: Into<PathBuf>,
        R: RngCore,
    {
        let this = Self {
            users: scc::HashMap::new(),
            tokens: scc::HashIndex::new(),
            root_dir: root_dir.into().into_boxed_path().into(),
            root_token: gen_token(rng),
            dirty: AtomicBool::new(false),
        };
        tracing::info!(
            "token of root account generated for this session: {}",
            this.root_token
        );
        this
    }

    /// Whether the user manager is empty.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// Loads all users from the filesystem.
    ///
    /// This function is blocking and _should only be called at initialization._
    ///
    /// # Errors
    ///
    /// - `Initialized` if the function manager is not empty.
    /// - Other errors if any error occurs while interacting with filesystem.
    pub fn read_from_fs(&self) -> Result<(), ManagerError> {
        let span = tracing::info_span!("loading users from the filesystem");
        let _e = span.enter();

        if !self.is_empty() {
            return Err(ManagerError::Initialized);
        }

        let file_result = std::fs::File::open(self.root_dir.join(USERS_FILE));
        if file_result
            .as_ref()
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        let serialized: SerializedUsers =
            serde_json::from_reader(std::io::BufReader::new(file_result?))?;

        self.users.reserve(serialized.users.len());
        let now = UtcDateTime::now();
        for user in serialized.users {
            for (token, time) in &user.tokens {
                if time > &now {
                    drop(self.tokens.insert_sync(token.clone(), user.name.clone()));
                }
            }
            self.users
                .insert_sync(user.name.clone(), user)
                .map_err(|_| ManagerError::Duplicated)?;
        }

        Ok(())
    }

    /// Writes all users to the filesystem.
    #[allow(clippy::missing_errors_doc)] // general I/O errors from std::io
    pub async fn write_all_to_fs(&self) -> Result<(), ManagerError> {
        let span = tracing::info_span!("writing users to the filesystem");
        let _e = span.enter();

        let mut users = Vec::with_capacity(self.users.len());
        self.users.iter_sync(|_, user| {
            users.push(user.clone());
            true
        });

        tokio::fs::create_dir_all(&self.root_dir).await?;
        tokio::fs::write(
            self.root_dir.join(USERS_FILE),
            serde_json::to_vec(&SerializedUsers {
                users: users.into_boxed_slice(),
            })?,
        )
        .await?;

        self.dirty.store(false, atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Adds a user to the manager.
    ///
    /// # Errors
    ///
    /// - `Duplicated` if a user with the same name already exists.
    pub fn add(&self, user: User) -> Result<(), ManagerError> {
        if user.name == ROOT_USERNAME {
            return Err(ManagerError::Duplicated);
        }

        self.users
            .insert_sync(user.name.clone(), user)
            .map_err(|_| ManagerError::Duplicated)?;

        self.mark_dirty();
        Ok(())
    }

    /// Authenticates a user.
    pub fn auth<'g, I>(&self, token: &str, groups: I) -> bool
    where
        I: IntoIterator<Item = Cow<'g, Group>>,
    {
        if self.root_token == token {
            return true;
        }

        self.tokens
            .peek_with(token, |_, un| {
                self.users.read_sync(un, |_, user| {
                    groups.into_iter().all(|g| user.groups.contains(&g))
                })
            })
            .flatten()
            .unwrap_or_default()
    }

    /// Peeks a user from given token, returning the value from given function or `None` if peeking a root account.
    ///
    /// # Errors
    ///
    /// - `NotFound` if the user does not exist.
    pub fn peek_from_token<F, U>(&self, token: &str, f: F) -> Result<Option<U>, ManagerError>
    where
        F: FnOnce(&User) -> U,
    {
        if token == self.root_token {
            return Ok(None);
        }

        self.tokens
            .peek_with(token, |_, un| {
                if un == ROOT_USERNAME {
                    Some(None)
                } else {
                    self.users.read_sync(un, |_, user| f(user)).map(Some)
                }
            })
            .flatten()
            .ok_or(ManagerError::NotFound)
    }

    /// Adds a randomly-generated token to this user and returns the token.
    ///
    /// # Errors
    ///
    /// - `NotFound` if the user does not exist.
    pub fn add_token<R>(
        &self,
        name: &str,
        rng: R,
        duration: Duration,
    ) -> Result<String, ManagerError>
    where
        R: RngCore,
    {
        let token = self
            .users
            .get_sync(name)
            .ok_or(ManagerError::NotFound)?
            .add_token(rng, duration);
        drop(self.tokens.insert_sync(token.clone(), name.to_owned()));
        self.mark_dirty();
        Ok(token)
    }

    /// Returns the name of the user holding the given token.
    pub fn user_name(&self, token: &str) -> Option<String> {
        if token == self.root_token {
            return Some("root".to_owned());
        }
        self.tokens.peek_with(token, |_, name| name.clone())
    }

    /// Removes a user from this manager.
    ///
    /// # Errors
    ///
    /// Returns an error if the user is not found.
    pub fn remove(&self, name: &str) -> Result<(), ManagerError> {
        self.users
            .remove_sync(name)
            .map(|_| ())
            .ok_or(ManagerError::NotFound)?;
        self.mark_dirty();
        Ok(())
    }

    /// Peeks an user or `None` if peeking a root account.
    ///
    /// # Errors
    ///
    /// Returns an error if the user is not found.
    #[doc(alias = "get")]
    pub fn peek<F, U>(&self, name: &str, f: F) -> Result<Option<U>, ManagerError>
    where
        F: FnOnce(&User) -> U,
    {
        if name == ROOT_USERNAME {
            return Ok(None);
        }
        self.users
            .read_sync(name, |_, user| f(user))
            .ok_or(ManagerError::NotFound)
            .map(Some)
    }

    /// Peeks an user mutably or `None` if peeking a root account.
    ///
    /// # Errors
    ///
    /// Returns an error if the user is not found.
    #[doc(alias = "get_mut")]
    pub fn peek_mut<F, U>(&self, name: &str, f: F) -> Result<Option<U>, ManagerError>
    where
        F: FnOnce(&mut User) -> U,
    {
        if name == ROOT_USERNAME {
            return Ok(None);
        }
        let mut user = self.users.get_sync(name).ok_or(ManagerError::NotFound)?;
        self.mark_dirty();
        Ok(Some(f(&mut user)))
    }
}

/// Errors that may occur when working with a [`UserManager`].
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum ManagerError {
    #[error("I/O error occurred: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parsing error: {0}")]
    ParseJson(#[from] serde_json::Error),
    #[error("the user manager is already initialized")]
    Initialized,
    #[error("the user holding the given name already exists")]
    Duplicated,
    #[error("the user holding the given name does not exist")]
    NotFound,
}
