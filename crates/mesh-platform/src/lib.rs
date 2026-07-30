//! Platform portability shims.
//!
//! A single home for the `libc` constants and types whose definitions differ
//! across the platforms mesh builds for. Centralizing them here keeps each
//! cfg-gated cast in one reviewed place instead of copied into every call site —
//! historically the source of repeated macOS-only build breaks.
//!
//! Linux and macOS are built and tested; FreeBSD is compile-checked in CI, which
//! is what keeps the `not(macos)` branches below honest for it.
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

/// The `TIOCGWINSZ` ioctl request, typed the same way and for the same reason.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCGWINSZ: libc::c_ulong = libc::TIOCGWINSZ as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCGWINSZ: libc::c_ulong = libc::TIOCGWINSZ;
#[cfg(target_env = "musl")]
pub const TIOCGWINSZ: libc::c_int = libc::TIOCGWINSZ;

/// The `TIOCSWINSZ` ioctl request — the write half of the pair above. Used by
/// the tests, which give a pty a known size to read back.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCSWINSZ: libc::c_ulong = libc::TIOCSWINSZ as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCSWINSZ: libc::c_ulong = libc::TIOCSWINSZ;
#[cfg(target_env = "musl")]
pub const TIOCSWINSZ: libc::c_int = libc::TIOCSWINSZ;
