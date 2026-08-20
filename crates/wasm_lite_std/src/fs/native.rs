// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native filesystem backend using blocking I/O on the async blocking pool.

use super::Priority;
use blocking::unblock;
use std::fmt;
use std::io::{Read, Seek};
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

/// Observation, compiled only when the crate's `logwise` feature is on.
///
/// Nothing here is reachable otherwise: not the clock read, not the field
/// expressions, not the call sites. Enabling the feature still emits nothing
/// until a runtime installs a dispatcher.
#[cfg(feature = "logwise")]
mod obs {
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// How long a blocking fallback may run before it is worth a warning.
    const SLOW: Duration = Duration::from_millis(1);

    /// Names an `io::ErrorKind` as a stable, support-safe string.
    ///
    /// The kind is a std-defined discriminant that says nothing about the
    /// caller's data, unlike the message, which can quote a path.
    fn error_kind(error: &std::io::Error) -> &'static str {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::NotFound => "not_found",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::AlreadyExists => "already_exists",
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::InvalidData => "invalid_data",
            ErrorKind::UnexpectedEof => "unexpected_eof",
            ErrorKind::IsADirectory => "is_a_directory",
            ErrorKind::OutOfMemory => "out_of_memory",
            _ => "other",
        }
    }

    /// Reports a failed open.
    ///
    /// The path is caller-derived and could name anything, so it is `local` and
    /// a `detail` field: only a view that asked for local-only detail
    /// materializes it, and `Path::display` does its formatting in that sink
    /// rather than here.
    pub(super) fn open_failed(path: &Path, error: &std::io::Error) {
        let shown = path.display();
        logwise::event!(
            class: operational,
            severity: error,
            name: "wasm_lite_std.fs.open.failed",
            error_kind = support(error_kind(error)),
            detail path = local(logwise::ValueRef::display(&shown)),
        );
    }

    /// Reports a failure on an already-open handle, which has no path to name.
    pub(super) fn operation_failed(operation: &'static str, error: &std::io::Error) {
        logwise::event!(
            class: operational,
            severity: error,
            name: "wasm_lite_std.fs.operation.failed",
            operation = support(operation),
            error_kind = support(error_kind(error)),
        );
    }

    /// Reports that this platform satisfied an async call by handing it to a
    /// blocking thread pool, and how long that took.
    ///
    /// A `logwise::SpanGuard` would be the natural fit and is the wrong tool:
    /// it is `!Send` by design, and this guard is held across the `await` inside
    /// `async fn`s whose futures must stay `Send` — `fs`'s browser suite asserts
    /// exactly that. Holding an `Instant` keeps them `Send` and reports the same
    /// two things when the scope ends.
    pub(super) struct BlockingFallback {
        operation: &'static str,
        started: Instant,
    }

    impl BlockingFallback {
        pub(super) fn begin(operation: &'static str) -> Self {
            Self {
                operation,
                started: Instant::now(),
            }
        }
    }

    impl Drop for BlockingFallback {
        fn drop(&mut self) {
            let elapsed = self.started.elapsed();
            let duration_ns = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
            logwise::measurement!(
                "wasm_lite_std.fs.blocking_fallback",
                operation = support(self.operation),
                duration_ns = support(duration_ns),
            );
            if elapsed >= SLOW {
                logwise::event!(
                    class: performance,
                    severity: warn,
                    name: "wasm_lite_std.fs.blocking_fallback.slow",
                    operation = support(self.operation),
                    duration_ns = support(duration_ns),
                );
            }
        }
    }
}

#[derive(Debug)]
pub struct File(Arc<std::fs::File>);

#[derive(Debug)]
pub struct Data(Box<[u8]>);

#[derive(Debug, Clone)]
pub struct Metadata(std::fs::Metadata);

#[derive(Debug)]
pub struct Error(std::io::Error);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "I/O error: {}", self.0)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self(value)
    }
}

fn allocate_zeroed(len: usize) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            format!("could not allocate a {len}-byte file buffer: {error}"),
        )
    })?;
    data.resize(len, 0);
    Ok(data)
}

fn read_up_to(reader: &mut impl Read, len: usize) -> std::io::Result<Box<[u8]>> {
    let mut data = allocate_zeroed(len)?;
    let mut filled = 0;
    while filled < len {
        match reader.read(&mut data[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    data.truncate(filled);
    Ok(data.into_boxed_slice())
}

impl File {
    pub async fn open(path: impl AsRef<Path>, _priority: Priority) -> Result<Self, Error> {
        #[cfg(feature = "logwise")]
        let _blocking = obs::BlockingFallback::begin("open");
        let path = path.as_ref().to_owned();
        // The path travels into the pool and back out rather than being cloned,
        // so a failure can name it without costing an allocation on the success
        // path.
        let (opened, path) = unblock(move || {
            let opened = std::fs::File::open(&path);
            (opened, path)
        })
        .await;
        match opened {
            Ok(file) => Ok(Self(Arc::new(file))),
            Err(error) => {
                #[cfg(feature = "logwise")]
                obs::open_failed(&path, &error);
                let _ = path;
                Err(error.into())
            }
        }
    }

    pub async fn read(&self, len: usize, _priority: Priority) -> Result<Data, Error> {
        #[cfg(feature = "logwise")]
        let _blocking = obs::BlockingFallback::begin("read");
        let file = Arc::clone(&self.0);
        unblock(move || {
            let mut file = &*file;
            read_up_to(&mut file, len).map(Data)
        })
        .await
        .map_err(|error| {
            #[cfg(feature = "logwise")]
            obs::operation_failed("read", &error);
            error.into()
        })
    }

    pub async fn read_all(&self, _priority: Priority) -> Result<Data, Error> {
        #[cfg(feature = "logwise")]
        let _blocking = obs::BlockingFallback::begin("read_all");
        let file = Arc::clone(&self.0);
        unblock(move || {
            let len = usize::try_from(file.metadata()?.len()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "file is too large to fit in memory on this platform",
                )
            })?;
            let mut file = &*file;
            read_up_to(&mut file, len).map(Data)
        })
        .await
        .map_err(|error| {
            #[cfg(feature = "logwise")]
            obs::operation_failed("read_all", &error);
            error.into()
        })
    }

    pub async fn seek(
        &mut self,
        position: std::io::SeekFrom,
        _priority: Priority,
    ) -> Result<u64, Error> {
        #[cfg(feature = "logwise")]
        let _blocking = obs::BlockingFallback::begin("seek");
        let mut file = Arc::clone(&self.0);
        unblock(move || file.seek(position)).await.map_err(|error| {
            #[cfg(feature = "logwise")]
            obs::operation_failed("seek", &error);
            error.into()
        })
    }

    pub async fn metadata(&self, _priority: Priority) -> Result<Metadata, Error> {
        #[cfg(feature = "logwise")]
        let _blocking = obs::BlockingFallback::begin("metadata");
        let file = Arc::clone(&self.0);
        unblock(move || file.metadata().map(Metadata))
            .await
            .map_err(|error| {
                #[cfg(feature = "logwise")]
                obs::operation_failed("metadata", &error);
                error.into()
            })
    }
}

impl Data {
    pub fn into_boxed_slice(self) -> Box<[u8]> {
        self.0
    }
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for Data {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Data {}

impl std::hash::Hash for Data {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Metadata {
    pub fn len(&self) -> u64 {
        self.0.len()
    }
}

pub async fn exists(path: impl AsRef<Path>, _priority: Priority) -> bool {
    #[cfg(feature = "logwise")]
    let _blocking = obs::BlockingFallback::begin("exists");
    let path = path.as_ref().to_owned();
    unblock(move || path.exists()).await
}

pub fn set_default_origin(_origin: &'static str) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReader<'a> {
        data: &'a [u8],
        position: usize,
        chunk: usize,
    }

    impl Read for ShortReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let remaining = &self.data[self.position..];
            let len = remaining.len().min(output.len()).min(self.chunk);
            output[..len].copy_from_slice(&remaining[..len]);
            self.position += len;
            Ok(len)
        }
    }

    #[test]
    fn read_up_to_collects_short_reads_and_stops_at_eof() {
        let mut reader = ShortReader {
            data: b"short reads still make a complete file",
            position: 0,
            chunk: 3,
        };
        let data = read_up_to(&mut reader, 1024).unwrap();
        assert_eq!(&*data, reader.data);
    }
}
