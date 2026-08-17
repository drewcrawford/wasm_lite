// SPDX-License-Identifier: MIT OR Apache-2.0
//! Asynchronous, read-only file access for native and browser wasm.
//!
//! This is API-compatible with `wasm_lite_std::fs` on the wasm_lite backend.
//! `Priority` is a best-effort hint; no ordering guarantee is made.

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

pub type Priority = priority::Priority;

pub fn set_default_origin(origin: &'static str) {
    sys::set_default_origin(origin);
}

#[derive(Debug)]
pub struct File(sys::File);

impl File {
    pub async fn open(path: impl AsRef<Path>, priority: Priority) -> Result<Self, Error> {
        sys::File::open(path, priority)
            .await
            .map(Self)
            .map_err(Error)
    }

    pub async fn read(&self, len: usize, priority: Priority) -> Result<Data, Error> {
        self.0.read(len, priority).await.map(Data).map_err(Error)
    }

    pub async fn read_all(&self, priority: Priority) -> Result<Data, Error> {
        self.0.read_all(priority).await.map(Data).map_err(Error)
    }

    pub async fn seek(
        &mut self,
        position: std::io::SeekFrom,
        priority: Priority,
    ) -> Result<u64, Error> {
        self.0.seek(position, priority).await.map_err(Error)
    }

    pub async fn metadata(&self, priority: Priority) -> Result<Metadata, Error> {
        self.0.metadata(priority).await.map(Metadata).map_err(Error)
    }
}

#[derive(Debug)]
pub struct Data(sys::Data);

impl Data {
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

#[derive(Debug, Clone)]
pub struct Metadata(sys::Metadata);

impl Metadata {
    pub fn len(&self) -> u64 {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

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

pub async fn exists(path: impl AsRef<Path>, priority: Priority) -> bool {
    sys::exists(path, priority).await
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn public_types_are_send_sync_unpin() {
        fn assert_traits<T: Send + Sync + Unpin>() {}
        assert_traits::<File>();
        assert_traits::<Data>();
        assert_traits::<Metadata>();
        assert_traits::<Error>();
    }
}
