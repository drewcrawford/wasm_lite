// SPDX-License-Identifier: MIT OR Apache-2.0
//! Asynchronous, read-only file access for native and browser wasm.
//!
//! On native targets paths name operating-system files. On wasm32 they name
//! HTTP resources relative to the current origin (or the origin installed with
//! [`set_default_origin`]). Servers should support `HEAD`, byte ranges, and
//! uncompressed responses. A server that ignores ranges still works, but the
//! client must discard the bytes before the requested position.
//!
//! Only one operation may be in flight for a [`File`] at a time. In particular,
//! concurrent reads may observe an unpredictable file position.
//!
//! `Priority` is a portable scheduling hint. The current native and wasm
//! backends accept it for API compatibility but do not guarantee priority
//! ordering.
//!
//! # Example
//!
//! ```no_run
//! # // no_run because: requires an async runtime and a filesystem resource
//! # async fn example() -> Result<(), wasm_lite_std::fs::Error> {
//! use wasm_lite_std::fs::{File, Priority};
//!
//! let file = File::open("assets/config.bin", Priority::UserInitiated).await?;
//! let bytes = file.read_all(Priority::UserInitiated).await?;
//! println!("read {} bytes", bytes.len());
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
use native as sys;
#[cfg(target_arch = "wasm32")]
use wasm as sys;

/// A best-effort scheduling priority for a filesystem operation.
pub type Priority = priority::Priority;

/// Sets the base origin used by wasm file requests.
///
/// The configured value takes precedence over automatic origin detection. It
/// must be set before starting file operations. This is a no-op on native
/// targets so cross-platform startup code does not need a `cfg` branch.
pub fn set_default_origin(origin: &'static str) {
    sys::set_default_origin(origin);
}

/// A handle to an open, read-only file.
#[derive(Debug)]
pub struct File(sys::File);

impl File {
    /// Opens `path` for reading.
    pub async fn open(path: impl AsRef<Path>, priority: Priority) -> Result<Self, Error> {
        sys::File::open(path, priority)
            .await
            .map(Self)
            .map_err(Error)
    }

    /// Reads up to `buf_size` bytes at the current position and advances it.
    pub async fn read(&self, buf_size: usize, priority: Priority) -> Result<Data, Error> {
        self.0
            .read(buf_size, priority)
            .await
            .map(Data)
            .map_err(Error)
    }

    /// Reads from the current position through end of file.
    pub async fn read_all(&self, priority: Priority) -> Result<Data, Error> {
        self.0.read_all(priority).await.map(Data).map_err(Error)
    }

    /// Changes the position used by the next read.
    pub async fn seek(
        &mut self,
        position: std::io::SeekFrom,
        priority: Priority,
    ) -> Result<u64, Error> {
        self.0.seek(position, priority).await.map_err(Error)
    }

    /// Returns metadata for the file.
    pub async fn metadata(&self, priority: Priority) -> Result<Metadata, Error> {
        self.0.metadata(priority).await.map(Metadata).map_err(Error)
    }
}

/// Bytes returned by a file read.
#[derive(Debug)]
pub struct Data(sys::Data);

impl Data {
    /// Converts the data into an owned byte slice.
    pub fn into_boxed_slice(self) -> Box<[u8]> {
        self.0.into_boxed_slice()
    }
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl Deref for Data {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl From<Data> for Box<[u8]> {
    fn from(value: Data) -> Self {
        value.into_boxed_slice()
    }
}

impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}

impl Eq for Data {}

impl Hash for Data {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// File metadata.
#[derive(Debug, Clone)]
pub struct Metadata(sys::Metadata);

impl Metadata {
    /// Returns the file length in bytes.
    pub fn len(&self) -> u64 {
        self.0.len()
    }

    /// Returns whether the file length is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An error produced by a filesystem operation.
///
/// The wrapper deliberately keeps platform-specific errors private so the same
/// API remains usable with either backend.
#[derive(Debug)]
pub struct Error(sys::Error);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "async filesystem error: {}", self.0)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Tests whether a file or directory exists.
///
/// As with [`std::path::Path::exists`], errors are reported as `false`.
pub async fn exists(path: impl AsRef<Path>, priority: Priority) -> bool {
    sys::exists(path, priority).await
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::io::SeekFrom;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn assert_traits<T: Send + Sync + Unpin>() {}

    fn fixture_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wasm_lite_std-fs-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn public_types_are_send_sync_unpin() {
        assert_traits::<File>();
        assert_traits::<Data>();
        assert_traits::<Metadata>();
        assert_traits::<Error>();
    }

    #[test]
    fn native_file_contract() {
        let path = fixture_path();
        let bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();
        std::fs::write(&path, &bytes).unwrap();

        crate::block_on(async {
            let priority = Priority::unit_test();
            assert!(exists(&path, priority).await);
            assert!(!exists(path.with_extension("absent"), priority).await);

            let mut file = File::open(&path, priority).await.unwrap();
            let metadata = file.metadata(priority).await.unwrap();
            assert_eq!(metadata.len(), bytes.len() as u64);
            assert!(!metadata.is_empty());

            let first = file.read(257, priority).await.unwrap();
            let second = file.read(131, priority).await.unwrap();
            assert_eq!(&first[..], &bytes[..257]);
            assert_eq!(&second[..], &bytes[257..388]);

            assert_eq!(
                file.seek(SeekFrom::Current(-88), priority).await.unwrap(),
                300
            );
            assert_eq!(file.seek(SeekFrom::End(-96), priority).await.unwrap(), 4000);
            let tail = file.read_all(priority).await.unwrap();
            assert_eq!(&tail[..], &bytes[4000..]);
            assert!(file.read(1, priority).await.unwrap().is_empty());

            file.seek(SeekFrom::Start(0), priority).await.unwrap();
            let all: Box<[u8]> = file.read_all(priority).await.unwrap().into();
            assert_eq!(&*all, &bytes);
            assert!(file.read(usize::MAX, priority).await.is_err());
        });

        std::fs::remove_file(path).unwrap();
    }
}
