//! Linux-specific implementation.

use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{NonExhaustiveMarker, dnem, sandbox::SandboxConfig};

/// Extended configuration of a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfigExt {
    /// Allowlist or denylist mode of syscall filtering.
    #[serde(default)]
    pub syscall_filter_mode: SyscallFilterMode,
    /// List of syscall names to be filtered. See [`Self::syscall_filter_mode`] for filter mode.
    ///
    /// _Make sure the given names are valid for current architecture._
    pub syscall_filter: Box<[String]>,

    /// Whether to provide procfs at `/proc`.
    pub mount_procfs: bool,
    /// Whether to provide _a new_ devtmpfs at `/dev`.
    pub mount_devtmpfs: bool,
    /// Whether to provide _a new_ tmpfs at `/tmp`.
    pub mount_tmpfs: bool,

    #[doc(hidden)]
    #[serde(skip, default = "dnem")]
    pub __ne: NonExhaustiveMarker,
}

/// Mode of syscall filtering.
///
/// The default mode is [`SyscallFilterMode::Deny`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(clippy::exhaustive_enums)]
pub enum SyscallFilterMode {
    /// Allowlist mode.
    Allow,
    /// Denylist mode.
    #[default]
    Deny,
}

impl Default for SandboxConfigExt {
    fn default() -> Self {
        Self {
            syscall_filter_mode: SyscallFilterMode::Deny,
            syscall_filter: Box::default(),
            mount_procfs: true,
            mount_devtmpfs: true,
            mount_tmpfs: false,
            __ne: dnem(),
        }
    }
}

/// Bubblewrap-based sandbox implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bubblewrap;

impl crate::sandbox::Sandbox for Bubblewrap {
    type Handle = tokio::process::Child;

    async fn spawn(
        &self,
        config: &SandboxConfig,
        contents_path: &Path,
    ) -> std::io::Result<Self::Handle> {
        const COMMAND_BUBBLEWRAP: &str = "bwrap";

        let args = bwrap_args(
            config,
            contents_path,
            #[cfg(all(feature = "seccomp", target_os = "linux"))]
            {
                || -> std::io::Result<std::os::fd::OwnedFd> {
                    use std::os::fd::{AsFd as _, OwnedFd};

                    let (r, w) = std::io::pipe()?;
                    compile_seccomp_filter(config, w.as_fd()).map_err(std::io::Error::other)?;
                    drop(w);
                    Ok(OwnedFd::from(r))
                }()
                .inspect_err(|e| {
                    tracing::error!("failed to create pipe and compile seccomp filter: {e}")
                })
                .ok()
            },
        );
        let stdio = || {
            if config.inherit_stdout {
                std::process::Stdio::inherit()
            } else {
                std::process::Stdio::null()
            }
        };

        let mut command = tokio::process::Command::new(COMMAND_BUBBLEWRAP);
        command
            .current_dir(contents_path)
            .args(args.iter().map(|cow| &**cow))
            .stdout(stdio())
            .stderr(stdio());
        tracing::info!(
            "spawning bubblewrap with args: {:?}",
            OsString::from_iter(
                command
                    .as_std()
                    .get_args()
                    .flat_map(|arg| [arg, " ".as_ref()])
            )
        );
        command.spawn()
    }
}

#[cfg(all(feature = "seccomp", target_os = "linux"))]
fn compile_seccomp_filter(
    config: &SandboxConfig,
    fd_w: std::os::fd::BorrowedFd<'_>,
) -> Result<(), libseccomp::error::SeccompError> {
    use libseccomp::{ScmpAction, ScmpArch, ScmpFilterContext, ScmpSyscall};

    const DENY_BEHAVIOR: ScmpAction = ScmpAction::Errno(libc::EPERM);

    let mut fcx = ScmpFilterContext::new(match config.platform_ext.syscall_filter_mode {
        // in reversed order to make difference between rules
        SyscallFilterMode::Deny => ScmpAction::Allow,
        SyscallFilterMode::Allow => DENY_BEHAVIOR,
    })?;

    let action = match config.platform_ext.syscall_filter_mode {
        SyscallFilterMode::Allow => ScmpAction::Allow,
        SyscallFilterMode::Deny => DENY_BEHAVIOR,
    };

    fcx.add_arch(ScmpArch::native())?;
    for name in &config.platform_ext.syscall_filter {
        let syscall = ScmpSyscall::from_name(name)?;
        fcx.add_rule(action, syscall)?;
    }
    fcx.export_bpf(fd_w)
}

fn bwrap_args<'a>(
    config: &'a SandboxConfig,
    contents_path: &'a Path,
    #[cfg(all(feature = "seccomp", target_os = "linux"))] bpf_fd: Option<std::os::fd::OwnedFd>,
) -> Vec<Cow<'a, OsStr>> {
    let _ = contents_path;

    // const ARG_CHDIR: &str = "--chdir";
    const ARG_UNSHARE_ALL: &str = "--unshare-all";
    const ARG_SHARE_NET: &str = "--share-net";
    const ARG_RO_BIND: &str = "--ro-bind";
    const ARG_RO_BIND_TRY: &str = "--ro-bind-try";
    const ARG_NEW_SESSION: &str = "--new-session";
    const ARG_SET_ENV: &str = "--setenv";
    const ARG_UNSET_ENV: &str = "--unsetenv";
    const ARG_DIE_WITH_PARENT: &str = "--die-with-parent";
    const ARG_PROC: &str = "--proc";
    const ARG_DEV: &str = "--dev";
    const ARG_TMPFS: &str = "--tmpfs";
    const ARG_CHDIR: &str = "--chdir";

    const MOUNT_POINT_PROCFS: &str = "/proc";
    const MOUNT_POINT_DEVTMPFS: &str = "/dev";
    const MOUNT_POINT_TMPFS: &str = "/tmp";
    const MOUNT_POINT_CONTENTS: &str = "/.__private_yfass_contents";

    let mut args = vec![
        // change directory to the contents path
        // Cow::Borrowed(ARG_CHDIR.as_ref()),
        // Cow::Borrowed(contents_path.as_os_str()),
        // no longer required as the bwrap execution directory is already the contents path

        // restrict namespaces
        Cow::Borrowed(ARG_UNSHARE_ALL.as_ref()),
        Cow::Borrowed(ARG_SHARE_NET.as_ref()),
        // create a new terminal session
        Cow::Borrowed(ARG_NEW_SESSION.as_ref()),
        // bind contents path as read-only
        Cow::Borrowed(ARG_RO_BIND.as_ref()), // this should not fail
        Cow::Borrowed("./".as_ref()),
        Cow::Borrowed(MOUNT_POINT_CONTENTS.as_ref()),
        Cow::Borrowed(ARG_CHDIR.as_ref()),
        Cow::Borrowed(MOUNT_POINT_CONTENTS.as_ref()),
        // die with parent process
        Cow::Borrowed(ARG_DIE_WITH_PARENT.as_ref()),
    ];

    // mount in-memory or real time filesystems
    if config.platform_ext.mount_procfs {
        args.extend_from_slice(&[
            Cow::Borrowed(ARG_PROC.as_ref()),
            Cow::Borrowed(MOUNT_POINT_PROCFS.as_ref()),
        ]);
    }
    if config.platform_ext.mount_devtmpfs {
        args.extend_from_slice(&[
            Cow::Borrowed(ARG_DEV.as_ref()),
            Cow::Borrowed(MOUNT_POINT_DEVTMPFS.as_ref()),
        ]);
    }
    if config.platform_ext.mount_tmpfs {
        args.extend_from_slice(&[
            Cow::Borrowed(ARG_TMPFS.as_ref()),
            Cow::Borrowed(MOUNT_POINT_TMPFS.as_ref()),
        ]);
    }

    // bind read-only entries
    args.extend(config.ro_entries.iter().flat_map(|(src, dst)| {
        let src = src.as_os_str();
        let dst = dst.as_deref().map(Path::as_os_str);
        [
            Cow::Borrowed(ARG_RO_BIND_TRY.as_ref()), // this may fail
            Cow::Borrowed(src),
            Cow::Borrowed(dst.unwrap_or(src)),
        ]
    }));

    // set environment variables
    for (k, v) in &config.envs {
        if let Some(v) = v {
            args.extend_from_slice(&[
                Cow::Borrowed(ARG_SET_ENV.as_ref()),
                Cow::Borrowed(k.as_ref()),
                Cow::Borrowed(v.as_ref()),
            ]);
        } else {
            args.extend_from_slice(&[
                Cow::Borrowed(ARG_UNSET_ENV.as_ref()),
                Cow::Borrowed(k.as_ref()),
            ]);
        }
    }

    // syscall filtering through seccomp
    #[cfg(all(feature = "seccomp", target_os = "linux"))]
    if let Some(bpf_fd) = bpf_fd {
        const ARG_SECCOMP: &str = "--seccomp";

        use std::os::fd::IntoRawFd as _;
        let raw_fd = bpf_fd.into_raw_fd();
        args.extend_from_slice(&[
            Cow::Borrowed(ARG_SECCOMP.as_ref()),
            Cow::Owned(format!("{raw_fd}").into()),
        ]);
    }

    // the command to be executed
    args.extend_from_slice(&[
        Cow::Borrowed("--".as_ref()),
        Cow::Borrowed(config.command.as_ref()),
    ]);

    // CLI arguments
    args.extend(config.args.iter().map(|arg| Cow::Borrowed(arg.as_ref())));

    args
}
