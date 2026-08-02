//! `file:` URLs for local paths.
//!
//! Two callers want the same string: `OSC 7`, which tells a terminal where the
//! shell is, and the `:url` modifier, which hands a path to anything that takes a
//! URL. They share the encoder so a path cannot mean one thing to the terminal and
//! another to `link`.

use std::path::Path;

/// The `file://host/path` URL `OSC 7` carries, and what `:url` answers.
///
/// The host is what lets a terminal tell a local path from one inside an `ssh`
/// session; an empty host is a valid `file:` URL and the honest answer when the
/// system will not say what it is called.
///
/// Bytes outside RFC 3986's unreserved set are percent-encoded, `/` excepted so
/// the path keeps its structure. A path is bytes, not text — a file whose name is
/// not UTF-8 is still a file to open — so this encodes the raw bytes rather than
/// going through a lossy string first.
///
/// `path` must be **absolute**: the host and the path are concatenated, so a
/// relative one would run into the host (`file://hostfoo/bar`) rather than
/// producing a path at all. Callers absolutize first.
pub(crate) fn file_url(host: &[u8], path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut url = String::from("file://");
    percent_encode(host, &mut url);
    percent_encode(path.as_os_str().as_bytes(), &mut url);
    url
}

/// Percent-encode into `out`, leaving unreserved bytes and `/` as they are.
fn percent_encode(bytes: &[u8], out: &mut String) {
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
}

/// This host's name, or empty when it cannot be read.
///
/// `$env.HOSTNAME` is not consulted: it is not in the environment a login shell
/// is given on either platform mesh targets, and a stale exported copy would name
/// the wrong machine after an `ssh`, which is exactly the case the host field is
/// there to distinguish.
pub(crate) fn hostname() -> Vec<u8> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `gethostname` writes at most `buffer.len()` bytes through the
    // pointer, which is valid and writable for exactly that many.
    let read = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if read != 0 {
        return Vec::new();
    }
    // A name too long for the buffer is truncated without a NUL on some
    // platforms, so fall back to the whole buffer when there is none.
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    buffer[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::file_url;
    use std::path::PathBuf;

    #[test]
    fn a_file_url_leaves_the_unreserved_set_alone() {
        assert_eq!(
            file_url(b"host", &PathBuf::from("/home/user")),
            "file://host/home/user"
        );
        // `-`, `.`, `_` and `~` are unreserved, and `/` is the path's structure:
        // encoding any of them is legal but turns a readable URL into noise.
        assert_eq!(
            file_url(b"host", &PathBuf::from("/a-b/c.d/e_f/~g")),
            "file://host/a-b/c.d/e_f/~g"
        );
        // An empty host is a valid `file:` URL, and the honest answer when the
        // system will not say what it is called.
        assert_eq!(file_url(b"", &PathBuf::from("/")), "file:///");
    }

    #[test]
    fn a_file_url_encodes_everything_a_reader_could_misread() {
        // A space would end the URL, `%` would start an escape that is not one,
        // and `#` and `?` would begin a fragment or a query — all of which a
        // directory name is allowed to contain.
        assert_eq!(
            file_url(b"host", &PathBuf::from("/tmp/two words")),
            "file://host/tmp/two%20words"
        );
        assert_eq!(
            file_url(b"host", &PathBuf::from("/50%/a#b?c")),
            "file://host/50%25/a%23b%3Fc"
        );
        assert_eq!(
            file_url(b"host", &PathBuf::from("/tmp/café")),
            "file://host/tmp/caf%C3%A9"
        );
    }

    #[test]
    fn a_file_url_encodes_a_path_that_is_not_utf8() {
        // A file whose name is not text is still a file to open, so the bytes are
        // encoded as they are rather than replaced on the way through a `String`.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe"));
        assert_eq!(file_url(b"host", &path), "file://host/tmp/%FF%FE");
    }
}
