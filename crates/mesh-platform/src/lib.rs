//! Platform portability shims.
//!
//! A single home for the `libc` constants and types whose definitions differ
//! across mesh's supported platforms (Linux and macOS). Centralizing them here
//! keeps each cfg-gated cast in one reviewed place instead of copied into every
//! call site — historically the source of repeated macOS-only build breaks.
//!
//! Add new shims here as they come up rather than reintroducing a `#[cfg]` dance
//! at the call site.

/// The `TIOCSCTTY` ioctl request, typed for `libc::ioctl` on this platform.
///
/// `libc` defines the constant as a narrower `c_uint` on macOS but as a
/// `c_ulong` on Linux, so a direct `libc::ioctl(fd, libc::TIOCSCTTY, 0)` fails
/// to compile on one platform or the other. This exposes it already widened to
/// the request type that `ioctl` takes on the target, so callers just pass
/// `mesh_platform::TIOCSCTTY`.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCSCTTY: libc::c_ulong = libc::TIOCSCTTY as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCSCTTY: libc::c_ulong = libc::TIOCSCTTY;
#[cfg(target_env = "musl")]
pub const TIOCSCTTY: libc::c_int = libc::TIOCSCTTY;
