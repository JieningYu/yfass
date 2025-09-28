//! Function abstractions.

use std::{
    fmt::Display,
    hash::Hash,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncRead, task::JoinSet};
use tokio_tar::Archive as Tar;

use crate::{NonExhaustiveMarker, dnem, sandbox::SandboxConfig, user};

/// Information of a function for FASS platform to host and perform.
#[derive(Debug, Clone, Serialize)]
pub struct Function {
    /// Metadata of the function, managed by the services.
    pub meta: Metadata,
    /// Runtime configuration of the function.
    pub config: Config,
}

type FunctionCell = Arc<RwLock<Function>>;

/// Runtime configuration of a [`Function`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Required user group to modify this function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<user::Group>,

    /// Address this function is listening on for HTTP and WebSocket connections.
    pub addr: SocketAddr,

    /// Configuration of the sandbox.
    pub sandbox: SandboxConfig,

    #[doc(hidden)]
    #[serde(skip, default = "dnem")]
    pub __ne: NonExhaustiveMarker,
}

/// Metadata of a [`Function`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// The function's name.
    pub name: String,
    /// Version identifier of the function.
    pub version: String,
    /// Alias of the function's version for quick access in subdomains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_alias: Option<String>,

    #[doc(hidden)]
    #[serde(skip, default = "dnem")]
    pub __ne: NonExhaustiveMarker,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            group: None,
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            sandbox: SandboxConfig::default(),
            __ne: dnem(),
        }
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            version_alias: None,
            __ne: dnem(),
        }
    }
}

/// Owned version of [`Key`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct OwnedKey {
    /// Function name.
    pub name: String,
    /// Function version or alias.
    pub version: String,
}

impl OwnedKey {
    /// Converts this owned key into a borrowed one.
    #[inline]
    pub fn as_ref(&self) -> Key<'_> {
        Key {
            name: &self.name,
            version: &self.version,
        }
    }
}

impl Hash for OwnedKey {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state);
    }
}

impl Display for OwnedKey {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl FromStr for OwnedKey {
    type Err = ParseKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (n, v) = s.split_once('@').ok_or(ParseKeyError::MissingSeparator)?;
        Ok(Self {
            name: n.to_owned(),
            version: v.to_owned(),
        })
    }
}

impl scc::Equivalent<OwnedKey> for Key<'_> {
    #[inline]
    fn equivalent(&self, key: &OwnedKey) -> bool {
        self == &key.as_ref()
    }
}

impl<'de> Deserialize<'de> for OwnedKey {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = OwnedKey;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "a key with pattern 'name@version'")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<OwnedKey>().map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// Unique identifier of a function.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub struct Key<'a> {
    /// Function name.
    pub name: &'a str,
    /// Function version or alias.
    pub version: &'a str,
}

impl Key<'_> {
    /// Converts this borrowed key into its owned variant.
    #[inline]
    pub fn into_owned(self) -> OwnedKey {
        OwnedKey {
            name: self.name.to_owned(),
            version: self.version.to_owned(),
        }
    }

    /// Converts this borrowed key into a prefix for host names.
    #[inline]
    pub fn to_host_prefix(&self) -> String {
        format!("{}.{}", self.version, self.name)
    }
}

impl Display for Key<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Manager of all functions.
///
/// # Filesystem Layout
///
/// Each function is stored in a directory under the root directory, with the following structure:
///
/// ```text
/// - [[(dir) name@version]]
///   - metadata.json
///   - config.json
///   - (dir) contents
///     - ...
/// ```
///
/// Associated structures:
///
/// - `config.json` for [`Config`].
/// - `metadata.json` for [`Metadata`].
#[derive(Debug)]
pub struct FunctionManager {
    functions: scc::HashMap<OwnedKey, FunctionCell>,

    root_dir: Arc<Path>,
    dirty: AtomicBool,
}

const FILE_METADATA: &str = "metadata.json";
const FILE_CONFIG: &str = "config.json";
const DIR_CONTENTS: &str = "contents";

impl FunctionManager {
    fn mark_dirty(&self) {
        self.dirty.store(true, atomic::Ordering::Relaxed);
    }

    /// Checks whether the user manager is dirty and needs to be written to the filesystem.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(atomic::Ordering::Relaxed)
    }

    /// Creates an empty, uninitialized function manager.
    ///
    /// For loading functions from the filesystem, use [`Self::read_from_fs`].
    pub fn new<P>(root_dir: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self {
            functions: scc::HashMap::new(),
            root_dir: root_dir.into().into_boxed_path().into(),
            dirty: AtomicBool::new(false),
        }
    }

    /// Checks whether this function manager is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    /// Loads all functions from the filesystem.
    ///
    /// This function is blocking and _should only be called at initialization._
    ///
    /// # Errors
    ///
    /// - `Initialized` if the function manager is not empty.
    /// - Other errors if any error occurs while interacting with filesystem.
    #[allow(clippy::missing_panics_doc)] // should not panic
    pub fn read_from_fs(&self) -> Result<(), ManagerError> {
        let span = tracing::info_span!("loading information of functions from the filesystem");
        let _e = span.enter();

        let result = self.priv_read_from_fs();

        // emit not-found errors
        if result.as_ref().is_err_and(|err| {
            if let ManagerError::Io(io) = err {
                io.kind() == std::io::ErrorKind::NotFound
            } else {
                false
            }
        }) {
            Ok(())
        } else {
            result
        }
    }

    /// Writes all information of functions to the filesystem.
    #[allow(clippy::missing_errors_doc)] // general I/O errors from std::io
    pub async fn write_all_to_fs(&self) -> Result<(), ManagerError> {
        let span = tracing::info_span!("writing information of functions to the filesystem");
        let _e = span.enter();

        self.priv_write_all_to_fs().await?;

        self.dirty.store(false, atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Adds a function to the platform with given minimal information and stream of tarball.
    ///
    /// # Errors
    ///
    /// - Returns an error if the function with given key already exists.
    /// - Returns an error if the tarball is corrupted.
    pub async fn add_func<R>(
        &self,
        key: Key<'_>,
        init_group: Option<user::Group>,
        tarball: &mut Tar<R>,
    ) -> Result<(), ManagerError>
    where
        R: AsyncRead + Unpin,
    {
        self.priv_init_info(key, init_group)?;
        self.priv_write_contents(key, tarball).await?;
        self.mark_dirty();
        Ok(())
    }

    /// Modifies alias of a function.
    ///
    /// # Errors
    ///
    /// Returns an error if the function with given key is not found.
    #[inline]
    pub fn modify_alias(&self, key: Key<'_>, alias: Option<String>) -> Result<(), ManagerError> {
        self.priv_modify_alias(key, alias)?;
        self.mark_dirty();
        Ok(())
    }

    /// Modifies configuration of a function.
    ///
    /// # Errors
    ///
    /// Returns an error if the function with given key is not found.
    #[inline]
    pub fn modify_config(&self, key: Key<'_>, config: Config) -> Result<(), ManagerError> {
        self.priv_modify_config(key, config)?;
        self.mark_dirty();
        Ok(())
    }

    /// Removes a function from this manager.
    ///
    /// # Errors
    ///
    /// Returns an error if the function with given key is not found.
    #[inline]
    pub async fn remove_func(&self, key: Key<'_>) -> Result<(), ManagerError> {
        self.priv_remove_func(key).await?;
        self.mark_dirty();
        Ok(())
    }

    /// Returns the function information of given key if present.
    #[inline]
    pub fn get(&self, key: Key<'_>) -> Option<FunctionCell> {
        self.functions.read_sync(&key, |_, v| v.clone())
    }

    /// Returns the path to the `contents` directory of a function.
    pub fn contents_path(&self, key: Key<'_>) -> PathBuf {
        self.root_dir.join(key.to_string()).join(DIR_CONTENTS)
    }
}

// Implementation
impl FunctionManager {
    fn priv_read_from_fs(&self) -> Result<(), ManagerError> {
        if !self.is_empty() {
            return Err(ManagerError::Initialized);
        }

        for entry in std::fs::read_dir(&self.root_dir)?
            .inspect(|r| {
                if let Err(e) = r {
                    tracing::error!("failed to read directory entry: {e}")
                }
            })
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                let Ok(func) = || -> Result<Function, ManagerError> {
                    let metadata: Metadata = serde_json::from_reader(std::io::BufReader::new(
                        std::fs::File::open(path.join(FILE_METADATA))?,
                    ))?;

                    let config: Config = serde_json::from_reader(std::io::BufReader::new(
                        std::fs::File::open(path.join(FILE_CONFIG))?,
                    ))?;

                    Ok(Function {
                        meta: metadata,
                        config,
                    })
                }()
                .inspect_err(|e| tracing::error!("failed to load function information: {e}")) else {
                    continue;
                };

                let func = Arc::new(RwLock::new(func));
                let fr = func.try_read().unwrap(); // this won't fail

                if let Some(ref alias) = fr.meta.version_alias {
                    let _r = self
                        .functions
                        .insert_sync(
                            OwnedKey {
                                name: fr.meta.name.clone(),
                                version: alias.clone(),
                            },
                            func.clone(),
                        )
                        .inspect_err(|(k, _)| {
                            tracing::error!("duplicated function entry: (alias) {k}",)
                        });
                }

                let key = OwnedKey {
                    name: fr.meta.name.clone(),
                    version: fr.meta.version.clone(),
                };

                drop(fr);

                let _r = self
                    .functions
                    .insert_sync(key, func)
                    .inspect_err(|(k, _)| tracing::error!("duplicated function entry: {k}"));
            }
        }

        Ok(())
    }

    async fn priv_write_all_to_fs(&self) -> Result<(), ManagerError> {
        let mut js = JoinSet::new();

        self.functions.iter_sync(|key, func| {
            let func = func.clone();
            let key = key.clone();
            let path = self.root_dir.join(key.to_string());

            let func = func.read();
            let meta = serde_json::to_vec_pretty(&func.meta);
            let config = serde_json::to_vec_pretty(&func.config);

            js.spawn(async move {
                let _r: Result<(), ManagerError> = async {
                    tokio::fs::create_dir_all(&path).await?;
                    tokio::fs::write(path.join(FILE_METADATA), meta?).await?;
                    tokio::fs::write(path.join(FILE_CONFIG), config?).await?;

                    Ok(())
                }
                .await
                .inspect_err(|e| {
                    tracing::error!("failed to write function `{key}` to filesystem: {e}");
                });
            });
            true
        });

        drop(js.join_all().await);
        Ok(())
    }

    fn priv_modify_config(&self, key: Key<'_>, config: Config) -> Result<(), ManagerError> {
        let func = self
            .functions
            .read_sync(&key, |_, func| func.clone())
            .ok_or(ManagerError::NotFound)?;

        func.write().config = config;

        Ok(())
    }

    fn priv_modify_alias(&self, key: Key<'_>, alias: Option<String>) -> Result<(), ManagerError> {
        let func = self
            .functions
            .read_sync(&key, |_, func| func.clone())
            .ok_or(ManagerError::NotFound)?;

        let mut wg = func.write();
        if wg.meta.version_alias == alias {
            return Ok(());
        }
        let an = alias.is_some();
        let ao = std::mem::replace(&mut wg.meta.version_alias, alias);
        drop(wg);

        if let Some(old) = ao {
            self.priv_remove_alias(key, &old)?;
        }

        if an {
            self.priv_add_alias(&func)?;
        }

        Ok(())
    }

    async fn priv_remove_func(&self, key: Key<'_>) -> Result<(), ManagerError> {
        let (_, func) = self
            .functions
            .remove_sync(&key)
            .ok_or(ManagerError::NotFound)?;
        if let Some(ref alias) = func.read().meta.version_alias {
            self.priv_remove_alias(key, alias)?;
        }

        tokio::fs::remove_dir_all(self.root_dir.join(key.to_string())).await?;
        Ok(())
    }

    fn priv_remove_alias(&self, key: Key<'_>, old_alias: &str) -> Result<(), ManagerError> {
        // assume that the function with key is not aliased

        self.functions.remove_sync(&Key {
            name: key.name,
            version: old_alias,
        });
        Ok(())
    }

    fn priv_add_alias(&self, new_aliased: &FunctionCell) -> Result<(), ManagerError> {
        // assume that new_aliased is correctly aliased itself

        let nfr = new_aliased.read();
        let alias_key = Key {
            name: &nfr.meta.name,
            version: nfr
                .meta
                .version_alias
                .as_deref()
                .ok_or(ManagerError::NotAliased)?,
        };

        // update alias entry
        if let Some(mut entry_alias) = self.functions.get_sync(&alias_key) {
            *entry_alias = new_aliased.clone();
            let name = alias_key.name.to_owned();

            // forbid potential deadlocks
            drop(nfr);

            let old_key = OwnedKey {
                name,
                version: entry_alias.read().meta.version.clone(),
            };

            drop(entry_alias);

            // remove old entry's alias
            if let Some(old) = self.functions.read_sync(&old_key, |_, f| f.clone()) {
                old.write().meta.version_alias = None;
            }
        }

        Ok(())
    }

    async fn priv_write_contents<R>(
        &self,
        key: Key<'_>,
        tarball: &mut Tar<R>,
    ) -> Result<(), ManagerError>
    where
        R: AsyncRead + Unpin,
    {
        let path = self.contents_path(key);
        tokio::fs::create_dir_all(&path).await?;
        tarball.unpack(path).await?;
        Ok(())
    }

    fn priv_init_info(
        &self,
        key: Key<'_>,
        init_group: Option<user::Group>,
    ) -> Result<(), ManagerError> {
        let func = Function {
            meta: Metadata {
                name: key.name.to_owned(),
                version: key.version.to_owned(),
                ..Default::default()
            },

            config: Config {
                group: init_group,
                ..Default::default()
            },
        };

        let key = OwnedKey {
            name: func.meta.name.clone(),
            version: func.meta.version.clone(),
        };
        if let scc::hash_map::Entry::Vacant(entry) = self.functions.entry_sync(key) {
            let cell = Arc::new(RwLock::new(func));
            drop(entry.insert_entry(cell.clone()));
            Ok(())
        } else {
            Err(ManagerError::Duplicated)
        }
    }
}

/// Errors that may occur when working with a [`FunctionManager`].
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum ManagerError {
    #[error("the given function is not aliased")]
    NotAliased,
    #[error("I/O error occurred: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parsing error: {0}")]
    ParseJson(#[from] serde_json::Error),
    #[error("the function manager is already initialized")]
    Initialized,
    #[error("the function holding the given key (or alias) already exists")]
    Duplicated,
    #[error("the function holding the given key (or alias) does not exist")]
    NotFound,
}

/// Errors that may occur when parsing a function key from string.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum ParseKeyError {
    #[error("invalid function name format")]
    InvalidName,
    #[error("invalid function version format")]
    InvalidVersion,
    #[error("missing separator between name and version")]
    MissingSeparator,
}
