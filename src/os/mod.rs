//! Operating-system-specific implementations.

use crate::sandbox::{self, Sandbox};

#[cfg(target_os = "linux")]
pub mod linux;

/// An unimplemented fallback implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unimplemented;

impl Sandbox for Unimplemented {
    type Handle = Unimplemented;

    async fn spawn(
        &self,
        _: &sandbox::SandboxConfig,
        _: &std::path::Path,
    ) -> std::io::Result<Self::Handle> {
        unsupported()
    }
}

impl sandbox::Handle for Unimplemented {
    async fn kill(self) {
        unsupported()
    }
}

#[inline(always)]
fn unsupported() -> ! {
    panic!("unsupported platform")
}

#[cfg(not(target_os = "linux"))]
type __SandboxImpl = Unimplemented;

#[cfg(target_os = "linux")]
type __SandboxImpl = linux::Bubblewrap;

/// The default sandbox implementation on the current platform.
pub type SandboxImpl = __SandboxImpl;

/// The default sandbox handle implementation on the current platform.
pub type SandboxHandleImpl = <SandboxImpl as Sandbox>::Handle;
