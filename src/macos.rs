//! Linux sandboxing.
//!
//! This module implements sandboxing on macOS using `sandbox_init`.

use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::PathBuf;
use std::result::Result as StdResult;
use std::{fs, ptr};

use crate::error::{Error, Result};
use crate::{Exception, Sandbox};

/// Deny-all fallback rule.
static DEFAULT_RULE: &[u8] = b"\
(version 1)
(import \"system.sb\")

(deny default)
(allow mach*)
(allow ipc*)
(allow signal (target others))
(allow process-fork)
(allow sysctl*)
(allow system*)
(allow file-read-metadata)
(system-network)
";

/// macOS sandboxing based on Seatbelt.
pub struct MacSandbox {
    env_exceptions: Vec<String>,
    profile: Vec<u8>,
    full_env: bool,
}

impl Sandbox for MacSandbox {
    fn new() -> Self {
        Self { profile: DEFAULT_RULE.to_vec(), env_exceptions: Vec::new(), full_env: false }
    }

    fn add_exception(&mut self, exception: Exception) -> Result<&mut Self> {
        // Temporary buffer to hold intermediate writes.
        // Prevents errors from breaking the whole sandbox profile.
        let mut buffer = Vec::new();

        match exception {
            Exception::Read(path) => {
                buffer.write_all(b"(allow file-read* (subpath ")?;
                let escaped_path = escape_path(path)?;
                buffer.write_all(escaped_path.as_bytes())?;
                buffer.write_all(b"))\n")?;
            },
            Exception::WriteAndRead(path) => {
                self.add_exception(Exception::Read(path.clone()))?;

                buffer.write_all(b"(allow file-write* (subpath ")?;
                let escaped_path = escape_path(path)?;
                buffer.write_all(escaped_path.as_bytes())?;
                buffer.write_all(b"))\n")?;
            },
            Exception::ExecuteAndRead(path) => {
                self.add_exception(Exception::Read(path.clone()))?;

                buffer.write_all(b"(allow process-exec (subpath ")?;
                let escaped_path = escape_path(path)?;
                buffer.write_all(escaped_path.as_bytes())?;
                buffer.write_all(b"))\n")?;
            },
            Exception::Networking => {
                buffer.write_all(b"(allow network*)\n")?;
            },
            Exception::Environment(key) => {
                self.env_exceptions.push(key);
                return Ok(self);
            },
            Exception::FullEnvironment => {
                self.full_env = true;
                return Ok(self);
            },
        }
        self.profile.write_all(&buffer)?;
        Ok(self)
    }

    fn lock(self) -> Result<()> {
        // Remove environment variables.
        if !self.full_env {
            crate::restrict_env_variables(&self.env_exceptions);
        }

        let profile = CString::new(self.profile)
            .map_err(|_| Error::ActivationFailed("invalid profile".into()))?;

        let mut error = ptr::null_mut();
        let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };

        if result == 0 {
            Ok(())
        } else {
            unsafe {
                let error_text = CStr::from_ptr(error)
                    .to_str()
                    .map_err(|_| Error::ActivationFailed("sandbox_init failed".into()))?
                    .to_owned();
                sandbox_free_error(error);

                Err(Error::ActivationFailed(error_text))
            }
        }
    }
}

/// Escape a path: /tt/in\a"x -> "/tt/in\\a\"x"
fn escape_path(path: PathBuf) -> StdResult<String, Error> {
    // Canonicalize the incoming path to support relative paths.
    // The `subpath` action only allows absolute paths.
    let canonical_path = fs::canonicalize(&path).map_err(|_| Error::InvalidPath(path.clone()))?;

    let mut path_str =
        canonical_path.into_os_string().into_string().map_err(|_| Error::InvalidPath(path))?;
    // Paths in `subpath` expressions must not end with /.
    while path_str.ends_with('/') && path_str != "/" {
        String::pop(&mut path_str);
    }
    path_str = path_str.replace('"', r#"\""#);
    path_str = path_str.replace('\\', r#"\\"#);
    Ok(format!("\"{path_str}\""))
}

extern "C" {
    fn sandbox_init(profile: *const i8, flags: u64, errorbuf: *mut *mut i8) -> i32;
    fn sandbox_free_error(errorbuf: *mut i8);
}
