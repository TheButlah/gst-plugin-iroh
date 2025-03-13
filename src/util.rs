use futures::future;
use futures::prelude::*;
use gst::ErrorMessage;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;
use tokio::runtime;

#[macro_export]
macro_rules! gst_err (
    () => {
        |err| {
            gst::error_msg!(
                gst::ResourceError::Failed,
                ["{:?}", err]
            )
        }
    };
    ($msg:tt) => {
        |err| {
            gst::error_msg!(
                gst::ResourceError::Failed,
                [$msg, err]
            )
        }
    };
);

#[derive(Error, Debug)]
pub enum WaitError {
    #[error("Future aborted")]
    FutureAborted,
    #[error("Future returned an error: {0}")]
    FutureError(ErrorMessage),
}

pub static RUNTIME: LazyLock<runtime::Runtime> = LazyLock::new(|| {
    runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("gst-iroh-runtime")
        .build()
        .unwrap()
});

#[derive(Default)]
pub enum Canceller {
    #[default]
    None,
    Handle(future::AbortHandle),
    Cancelled,
}

impl Canceller {
    pub fn abort(&mut self) {
        if let Canceller::Handle(ref canceller) = *self {
            canceller.abort();
        }

        *self = Canceller::Cancelled;
    }
}

pub fn wait<F, T>(
    canceller_mutex: &Mutex<Canceller>,
    future: F,
    timeout: u32,
) -> Result<T, WaitError>
where
    F: Send + Future<Output = T>,
    T: Send + 'static,
{
    let mut canceller = canceller_mutex.lock().unwrap();
    if matches!(*canceller, Canceller::Cancelled) {
        return Err(WaitError::FutureAborted);
    } else if matches!(*canceller, Canceller::Handle(..)) {
        return Err(WaitError::FutureError(gst::error_msg!(
            gst::ResourceError::Failed,
            ["Old Canceller should not exist"]
        )));
    }
    let (abort_handle, abort_registration) = future::AbortHandle::new_pair();
    *canceller = Canceller::Handle(abort_handle);
    drop(canceller);

    let future = async {
        if timeout == 0 {
            Ok(future.await)
        } else {
            let res = tokio::time::timeout(Duration::from_secs(timeout.into()), future).await;

            match res {
                Ok(r) => Ok(r),
                Err(e) => Err(gst::error_msg!(
                    gst::ResourceError::Read,
                    ["Request timeout, elapsed: {}", e.to_string()]
                )),
            }
        }
    };

    let future = async {
        match future::Abortable::new(future, abort_registration).await {
            Ok(Ok(res)) => Ok(res),

            Ok(Err(err)) => Err(WaitError::FutureError(gst::error_msg!(
                gst::ResourceError::Failed,
                ["Future resolved with an error {:?}", err]
            ))),

            Err(future::Aborted) => Err(WaitError::FutureAborted),
        }
    };

    let res = RUNTIME.block_on(future);

    let mut canceller = canceller_mutex.lock().unwrap();
    if matches!(*canceller, Canceller::Cancelled) {
        return Err(WaitError::FutureAborted);
    }
    *canceller = Canceller::None;

    res
}
