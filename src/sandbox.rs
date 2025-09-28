//! Abstraction of sandbox backends.
//!
//! A sandbox serves the FASS platform should:
//!
//! - Provide *read-only access* to the specified filesystem endpoints. No write access reserved.
//! - Provide full access to network.
//! - Pass through environment variables, both in the host system and variables especially passed to the sandbox.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{NonExhaustiveMarker, dnem};

/// Configuration of a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Command to be executed in the sandbox.
    ///
    /// This is usually a path to the program.
    pub command: String,

    /// Arguments to be passed to the command.
    #[serde(default)]
    pub args: Box<[String]>,

    /// Read-only filesystem endpoints to be mounted in the sandbox.
    ///
    /// The key is the path in the host system, and the value is the path in the sandbox,
    /// or `None` to keep the same path.
    ///
    /// The functions' `contents` directory should be mounted read only as well
    /// despite not being listed here.
    #[serde(default)]
    pub ro_entries: HashMap<PathBuf, Option<PathBuf>>,

    /// External *environment variables overrides* to be passed to the sandbox.
    ///
    /// The key is the name of the variable, and the value is the value of the variable,
    /// or `None` to remove the (inherited) variable.
    #[serde(default)]
    pub envs: HashMap<String, Option<String>>,

    /// Whether to inherit stdout from the host system.
    #[serde(default)]
    pub inherit_stdout: bool,

    /// Platform-specific configuration extension of the sandbox.
    #[serde(flatten)]
    pub platform_ext: SandboxConfigExt,

    #[doc(hidden)]
    #[serde(skip, default = "dnem")]
    pub __ne: NonExhaustiveMarker,
}

#[cfg(target_os = "linux")]
type SandboxConfigExt = crate::os::linux::SandboxConfigExt;

#[cfg(not(target_os = "linux"))]
type SandboxConfigExt = SandboxConfigExtFallback;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[allow(unused)]
struct SandboxConfigExtFallback {}

/// Abstraction of a sandbox implementation.
pub trait Sandbox: Default {
    /// Handle type of the running sandbox task.
    type Handle: Handle;

    /// Spawns a new sandbox task.
    fn spawn(
        &self,
        config: &SandboxConfig,
        contents_path: &Path,
    ) -> impl Future<Output = std::io::Result<Self::Handle>> + Send;
}

/// Handle of a running sandbox.
pub trait Handle: 'static {
    /// Kills the underlying sandbox task.
    fn kill(self) -> impl Future<Output = ()> + Send;

    /// Whether this task is still running or not.
    #[inline]
    fn is_running(&self) -> bool {
        true
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: vec![].into_boxed_slice(),
            ro_entries: HashMap::new(),
            envs: HashMap::new(),
            inherit_stdout: false,
            platform_ext: Default::default(),
            __ne: dnem(),
        }
    }
}

impl Handle for tokio::process::Child {
    async fn kill(mut self) {
        drop(
            tokio::process::Child::kill(&mut self)
                .await
                .inspect_err(|e| tracing::error!("failed to kill sandbox process: {}", e)),
        )
    }

    #[inline]
    fn is_running(&self) -> bool {
        self.id().is_some()
    }
}
