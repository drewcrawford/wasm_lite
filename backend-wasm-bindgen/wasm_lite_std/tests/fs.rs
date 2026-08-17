// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(target_arch = "wasm32")]

use std::io::SeekFrom;
use wasm_bindgen_test::*;
use wasm_lite_std::fs::{self, File, Priority};

wasm_bindgen_test_configure!(run_in_browser);

const TEST_PAGE: &str = "/";
const ABSENT: &str = "/wasm-lite-std-fs-no-such-resource";

fn assert_send<T: Send>(_value: T) {}

#[wasm_bindgen_test(async)]
async fn public_contract_and_reads() {
    let priority = Priority::unit_test();
    assert_send(File::open(TEST_PAGE, priority));
    assert_send(fs::exists(TEST_PAGE, priority));

    assert!(fs::exists(TEST_PAGE, priority).await);
    assert!(!fs::exists(ABSENT, priority).await);
    let mut file = File::open(TEST_PAGE, priority).await.unwrap();
    let metadata = file.metadata(priority).await.unwrap();
    assert!(!metadata.is_empty());

    let first = file.read(16, priority).await.unwrap();
    assert_eq!(first.len(), 16);
    file.seek(SeekFrom::Start(0), priority).await.unwrap();
    assert_eq!(file.read(16, priority).await.unwrap(), first);

    file.seek(SeekFrom::End(-8), priority).await.unwrap();
    assert_eq!(file.read_all(priority).await.unwrap().len(), 8);
    assert!(file.read(1, priority).await.unwrap().is_empty());
}

#[cfg(target_feature = "atomics")]
#[wasm_bindgen_test(async)]
async fn a_started_file_future_can_move_between_realms() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let priority = Priority::unit_test();
    let (sender, receiver) = wasm_lite_std::mpsc::channel();
    let worker = wasm_lite_std::spawn(move || {
        let mut future = Box::pin(File::open(TEST_PAGE, priority));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        sender.send_sync(future).unwrap();
    });

    let file = receiver.recv_async().await.unwrap().await.unwrap();
    assert!(!file.metadata(priority).await.unwrap().is_empty());
    worker.join_async().await.unwrap();
}
