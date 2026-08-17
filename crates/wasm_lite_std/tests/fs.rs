// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser contract tests for `wasm_lite_std::fs`.

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod suite {
    use std::io::SeekFrom;
    use wasm_lite_std::fs::{self, File, Priority};

    const SELF_WASM: &str = "/program.wasm";
    const ABSENT: &str = "/wasm-lite-std-fs-no-such-resource";

    fn assert_send<T: Send>(_value: T) {}
    fn assert_send_sync_unpin<T: Send + Sync + Unpin>() {}

    #[wasm_lite::wasm_lite_test]
    fn public_contract_and_range_reads() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let priority = Priority::unit_test();
            assert_send_sync_unpin::<File>();
            assert_send_sync_unpin::<fs::Data>();
            assert_send_sync_unpin::<fs::Metadata>();
            assert_send_sync_unpin::<fs::Error>();
            assert_send(File::open(SELF_WASM, priority));
            assert_send(fs::exists(SELF_WASM, priority));

            assert!(fs::exists(SELF_WASM, priority).await);
            assert!(!fs::exists(ABSENT, priority).await);

            let mut file = File::open(SELF_WASM, priority).await.unwrap();
            let metadata = file.metadata(priority).await.unwrap();
            assert!(metadata.len() > 16);
            assert!(!metadata.is_empty());

            let first = file.read(8, priority).await.unwrap();
            assert_eq!(&first[..4], b"\0asm");
            assert_eq!(first.len(), 8);

            let second = file.read(17, priority).await.unwrap();
            assert_eq!(second.len(), 17);
            assert_eq!(
                file.seek(SeekFrom::Current(-17), priority).await.unwrap(),
                8
            );
            assert_eq!(file.read(17, priority).await.unwrap(), second);

            file.seek(SeekFrom::End(-8), priority).await.unwrap();
            assert_eq!(file.read_all(priority).await.unwrap().len(), 8);
            assert!(file.read(1, priority).await.unwrap().is_empty());

            file.seek(SeekFrom::Start(0), priority).await.unwrap();
            assert_eq!(
                file.read_all(priority).await.unwrap().len(),
                metadata.len() as usize
            );
        });
    }

    /// Poll on a worker until the fetch has been pinned there, then move the
    /// public future to the main realm. Completion proves JS state stayed on
    /// the worker while the observer itself remained genuinely `Send`.
    #[cfg(target_feature = "atomics")]
    #[wasm_lite::wasm_lite_test]
    fn a_started_file_future_can_move_between_realms() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            use std::future::Future;
            use std::task::{Context, Poll, Waker};

            let priority = Priority::unit_test();
            let (sender, mut receiver) = wasm_lite_std::mpsc::channel();
            let worker = wasm_lite_std::spawn(move || {
                let mut future = Box::pin(File::open(SELF_WASM, priority));
                let mut context = Context::from_waker(Waker::noop());
                assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
                sender.send_sync(future).unwrap();
            });

            let future = receiver.recv_async().await.unwrap();
            let file = future.await.unwrap();
            assert!(file.metadata(priority).await.unwrap().len() > 16);
            worker.join_async().await.unwrap();
        });
    }
}
