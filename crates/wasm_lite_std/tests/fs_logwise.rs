// SPDX-License-Identifier: MIT OR Apache-2.0
//! The observation contract for [`wasm_lite_std::fs`]: which events exist, what
//! policy each field carries, and what a view that asked for less is *not*
//! given.
//!
//! The dispatcher is written out by hand rather than taken from a logwise
//! runtime — this crate depends only on the facade, so the test does too, and
//! the ABI being asserted is small. `install_dispatcher` is install-once per
//! process, so every case lives in this one binary.
//!
//! `harness = false`, matching `tests/fs.rs`: libtest does not run on
//! wasm32-unknown-unknown. Unlike that file, the native half is not a no-op —
//! the native backend is where the blocking pool and the path field live.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use logwise::{
    Class, Detail, Dispatch, EventRef, Interest, Kind, Metadata, Privacy, Severity, ValueRef,
};

/// One materialized field, or the fact that it was withheld.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub privacy: Privacy,
    pub detail: Detail,
    /// `None` means the call site declined to materialize it for this view.
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub name: &'static str,
    pub class: Class,
    pub kind: Kind,
    pub severity: Severity,
    pub fields: Vec<Field>,
}

static EVENTS: Mutex<Vec<Observed>> = Mutex::new(Vec::new());
/// What the recorder claims to want. Swapping it is what proves the call sites
/// consult the interest mask instead of materializing unconditionally.
static WANTED: Mutex<Interest> = Mutex::new(Interest::NONE);
static GENERATION: AtomicUsize = AtomicUsize::new(0);
static SERIALIZE: Mutex<()> = Mutex::new(());

struct Recorder;

impl Dispatch for Recorder {
    fn generation(&self) -> usize {
        GENERATION.load(Ordering::Acquire)
    }

    fn interest(&self, _metadata: &'static Metadata) -> Interest {
        *WANTED.lock().unwrap()
    }

    fn emit(&self, event: EventRef<'_>) {
        // Zipping the static schema against what was materialized is what makes
        // "withheld" observable: a declined field keeps its slot and its policy.
        let fields = event
            .metadata
            .fields
            .iter()
            .zip(event.fields.iter())
            .map(|(metadata, materialized)| Field {
                name: metadata.name,
                privacy: metadata.privacy,
                detail: metadata.detail,
                value: materialized.map(|field| match field.value {
                    ValueRef::Str(value) => value.to_string(),
                    other => format!("{other:?}"),
                }),
            })
            .collect();
        EVENTS.lock().unwrap().push(Observed {
            name: event.metadata.event_name,
            class: event.metadata.class,
            kind: event.metadata.kind,
            severity: event.metadata.severity,
            fields,
        });
    }
}

static RECORDER: Recorder = Recorder;

/// Holds the recorder to one interest mask for the duration of one case.
///
/// A guard rather than a closure wrapper so an `async` case can `.await` in the
/// middle without nesting one `block_on` inside another.
pub struct Session {
    _serialized: std::sync::MutexGuard<'static, ()>,
}

impl Session {
    pub fn events(self) -> Vec<Observed> {
        EVENTS.lock().unwrap().clone()
    }
}

/// Starts a case with the recorder reporting exactly `wanted`.
///
/// Changing the mask advances the generation, or the call sites would keep
/// serving the interest they cached during the previous case.
pub fn session(wanted: Interest) -> Session {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        logwise::install_dispatcher(&RECORDER).expect("install dispatcher");
    });
    let serialized = SERIALIZE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *WANTED.lock().unwrap() = wanted;
    GENERATION.fetch_add(1, Ordering::Release);
    EVENTS.lock().unwrap().clear();
    Session {
        _serialized: serialized,
    }
}

pub fn everything() -> Interest {
    Interest::CORE_SUPPORT
        .union(Interest::CORE_LOCAL)
        .union(Interest::CORE_SECRET)
        .union(Interest::DETAIL_SUPPORT)
        .union(Interest::DETAIL_LOCAL)
        .union(Interest::DETAIL_SECRET)
}

pub fn support_only() -> Interest {
    Interest::CORE_SUPPORT.union(Interest::DETAIL_SUPPORT)
}

pub fn find<'a>(events: &'a [Observed], name: &str) -> &'a Observed {
    events
        .iter()
        .find(|event| event.name == name)
        .unwrap_or_else(|| panic!("no {name} in {events:?}"))
}

pub fn field<'a>(event: &'a Observed, name: &str) -> &'a Field {
    event
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| panic!("no field {name} in {event:?}"))
}

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    native::run();
}

// -- native ----------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use wasm_lite_std::block_on;
    use wasm_lite_std::fs::{self, File, Priority};

    const MISSING: &str = "/nonexistent/wasm_lite_std/fs/instrumentation/target";

    /// `harness = false`, so this file is its own runner. Each case panics on
    /// failure, which fails the binary and therefore the test.
    pub fn run() {
        for (name, case) in [
            (
                "failed_open_reports_a_stable_event",
                failed_open_reports_a_stable_event as fn(),
            ),
            (
                "a_support_view_never_sees_the_path",
                a_support_view_never_sees_the_path as fn(),
            ),
            (
                "a_core_only_view_never_sees_the_detail",
                a_core_only_view_never_sees_the_detail as fn(),
            ),
            (
                "an_uninterested_view_sees_nothing",
                an_uninterested_view_sees_nothing as fn(),
            ),
            (
                "failed_read_names_the_operation",
                failed_read_names_the_operation as fn(),
            ),
            (
                "the_blocking_fallback_is_measured",
                the_blocking_fallback_is_measured as fn(),
            ),
            ("open_futures_stay_send", open_futures_stay_send as fn()),
        ] {
            case();
            println!("test {name} ... ok");
        }
    }

    /// A failed open is a stable operational event. The path is caller-derived,
    /// so it is local-only *and* a detail field.
    fn failed_open_reports_a_stable_event() {
        let session = session(everything());
        let result = block_on(File::open(MISSING, Priority::unit_test()));
        let events = session.events();
        assert!(result.is_err());

        let failure = find(&events, "wasm_lite_std.fs.open.failed");
        assert_eq!(failure.class, Class::Operational);
        assert_eq!(failure.kind, Kind::Event);
        assert_eq!(failure.severity, Severity::Error);

        let kind = field(failure, "error_kind");
        assert_eq!(kind.privacy, Privacy::SupportSafe);
        assert_eq!(kind.detail, Detail::Core);
        assert_eq!(kind.value.as_deref(), Some("not_found"));

        let path = field(failure, "path");
        assert_eq!(path.privacy, Privacy::LocalOnly);
        assert_eq!(path.detail, Detail::Detail);
        assert_eq!(path.value.as_deref(), Some(MISSING));
    }

    /// A support-safe view learns that an open failed with `not_found`, and
    /// never learns which path.
    fn a_support_view_never_sees_the_path() {
        let session = session(support_only());
        let _ = block_on(File::open(MISSING, Priority::unit_test()));
        let events = session.events();

        let failure = find(&events, "wasm_lite_std.fs.open.failed");
        assert_eq!(
            field(failure, "error_kind").value.as_deref(),
            Some("not_found")
        );
        assert_eq!(
            field(failure, "path").value,
            None,
            "a local-only field must be withheld from a support view"
        );
    }

    /// Core-only withholds the detail field even though its privacy would have
    /// allowed it: the two axes are independent.
    fn a_core_only_view_never_sees_the_detail() {
        let session = session(Interest::CORE_SUPPORT.union(Interest::CORE_LOCAL));
        let _ = block_on(File::open(MISSING, Priority::unit_test()));
        let events = session.events();

        let failure = find(&events, "wasm_lite_std.fs.open.failed");
        assert!(field(failure, "error_kind").value.is_some());
        assert_eq!(field(failure, "path").value, None);
    }

    /// Nothing at all when no view is listening: the call sites check interest
    /// before they evaluate a field expression.
    fn an_uninterested_view_sees_nothing() {
        let session = session(Interest::NONE);
        let result = block_on(File::open(MISSING, Priority::unit_test()));
        let events = session.events();
        assert!(result.is_err());
        assert!(events.is_empty(), "expected silence, got {events:?}");
    }

    /// A failure on an already-open handle has no path to name, so it reports
    /// the operation instead.
    fn failed_read_names_the_operation() {
        let session = session(everything());
        let result = block_on(async {
            let file = File::open("/dev/zero", Priority::unit_test())
                .await
                .expect("open /dev/zero");
            // `usize::MAX` bytes cannot be allocated, and the failure comes back
            // from inside the blocking pool.
            file.read(usize::MAX, Priority::unit_test()).await
        });
        let events = session.events();
        assert!(result.is_err());

        let failure = find(&events, "wasm_lite_std.fs.operation.failed");
        assert_eq!(failure.severity, Severity::Error);
        let operation = field(failure, "operation");
        assert_eq!(operation.value.as_deref(), Some("read"));
        assert_eq!(
            operation.privacy,
            Privacy::SupportSafe,
            "an operation name is a constant from this crate"
        );
    }

    /// Every native call hands its work to a blocking pool, and says so.
    fn the_blocking_fallback_is_measured() {
        let session = session(everything());
        let exists = block_on(fs::exists("/dev/zero", Priority::unit_test()));
        let events = session.events();
        assert!(exists);

        let measurement = find(&events, "wasm_lite_std.fs.blocking_fallback");
        assert_eq!(measurement.kind, Kind::Measurement);
        assert_eq!(measurement.class, Class::Metric);
        assert_eq!(
            field(measurement, "operation").value.as_deref(),
            Some("exists")
        );
        assert!(field(measurement, "duration_ns").value.is_some());
    }

    /// The reason the blocking guard holds an `Instant` rather than a
    /// `logwise::SpanGuard`: a `SpanGuard` is `!Send` by design, and it would be
    /// held across the `await` below, so using one would make every one of these
    /// futures unspawnable. This is the regression test for that.
    fn open_futures_stay_send() {
        fn assert_send<T: Send>(_value: T) {}
        assert_send(File::open(MISSING, Priority::unit_test()));
        assert_send(fs::exists(MISSING, Priority::unit_test()));
    }
}

// -- wasm ------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use wasm_lite_std::fs::{File, Priority};

    /// A resource the runner's server does not have.
    const MISSING: &str = "/wasm-lite-std-fs-logwise-no-such-resource";

    /// A non-OK HTTP response is a stable operational event. The status code is
    /// server-defined and support-safe; the URL and the server's status text are
    /// caller-derived and local-only detail.
    #[wasm_lite::wasm_lite_test]
    fn failed_request_reports_a_stable_event() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let session = session(everything());
            let result = File::open(MISSING, Priority::unit_test()).await;
            let events = session.events();
            assert!(result.is_err());

            let failure = find(&events, "wasm_lite_std.fs.request.failed");
            assert_eq!(failure.class, Class::Operational);
            assert_eq!(failure.kind, Kind::Event);
            assert_eq!(failure.severity, Severity::Error);

            let operation = field(failure, "operation");
            assert_eq!(operation.privacy, Privacy::SupportSafe);
            assert_eq!(operation.value.as_deref(), Some("open"));

            let status = field(failure, "status");
            assert_eq!(status.privacy, Privacy::SupportSafe);
            assert_eq!(status.detail, Detail::Core);
            assert_eq!(status.value.as_deref(), Some("404"));

            let url = field(failure, "url");
            assert_eq!(url.privacy, Privacy::LocalOnly);
            assert_eq!(url.detail, Detail::Detail);
            assert!(url.value.as_deref().expect("url").ends_with(MISSING));

            let status_text = field(failure, "status_text");
            assert_eq!(status_text.privacy, Privacy::LocalOnly);
            assert_eq!(status_text.detail, Detail::Detail);
        });
    }

    /// A support-safe view gets the status code and neither the URL nor the
    /// server's text; a core-only view gets neither detail field; an
    /// uninterested view gets nothing at all.
    #[wasm_lite::wasm_lite_test]
    fn narrower_views_are_withheld_the_details() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            // Each `let` shadows the previous guard, which drops it and
            // releases the serializing lock before the next case takes it.
            let support = session(support_only());
            let _ = File::open(MISSING, Priority::unit_test()).await;
            let events = support.events();
            let failure = find(&events, "wasm_lite_std.fs.request.failed");
            assert!(field(failure, "status").value.is_some());
            assert_eq!(field(failure, "url").value, None);
            assert_eq!(field(failure, "status_text").value, None);

            let core_only = session(Interest::CORE_SUPPORT.union(Interest::CORE_LOCAL));
            let _ = File::open(MISSING, Priority::unit_test()).await;
            let events = core_only.events();
            let failure = find(&events, "wasm_lite_std.fs.request.failed");
            assert!(field(failure, "status").value.is_some());
            assert_eq!(field(failure, "url").value, None);

            let uninterested = session(Interest::NONE);
            let _ = File::open(MISSING, Priority::unit_test()).await;
            let events = uninterested.events();
            assert!(events.is_empty(), "expected silence, got {events:?}");
        });
    }
}
