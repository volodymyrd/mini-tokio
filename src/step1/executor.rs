// Step 1: block_on
//
// Run a future to completion on the calling thread.
//
// Rules you must follow:
//   1. The future must be pinned before the first poll (futures are self-referential).
//   2. You must construct a Context from a Waker before calling poll.
//   3. After Poll::Pending you must park — not spin — so you don't burn CPU.
//   4. After waking up you must loop back and poll again (spurious wakeups are real).
//
// TODO: implement block_on.
//
// Skeleton:
//
//   pub fn block_on<F: Future>(future: F) -> F::Output {
//       pin the future            (std::pin::pin! macro or Box::pin)
//       build a Waker             (call super::waker::thread_waker)
//       build a Context           (Context::from_waker)
//       loop {
//           match pinned.as_mut().poll(&mut cx) {
//               Poll::Ready(val)  => return val,
//               Poll::Pending     => thread::park(),
//           }
//       }
//   }

use std::future::Future;
use std::task::Context;
use std::task::Poll;
use std::thread;

use super::waker::thread_waker;
use core::pin::pin;
/// Run `future` to completion, blocking the current thread.
///
/// This is the simplest possible async executor: no task queue, no spawning,
/// just one future and a waker that calls `thread::unpark`.
///
/// # Panics
///
/// Does not panic under normal use. If the future itself panics, that panic
/// propagates to the caller (standard Rust unwind behaviour).
///
/// # Example
///
/// ```
/// use mini_tokio::step1::block_on;
///
/// let result = block_on(async { 1 + 1 });
/// assert_eq!(result, 2);
/// ```
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut pinned = pin!(future);
    let waker = thread_waker(thread::current());
    let mut context = Context::from_waker(&waker);
    loop {
        match pinned.as_mut().poll(&mut context) {
            Poll::Ready(val) => return val,
            Poll::Pending => thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::Poll;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// A future that completes immediately with a value.
    struct Ready<T>(Option<T>);

    impl<T: Unpin> Future for Ready<T> {
        type Output = T;
        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<T> {
            Poll::Ready(self.0.take().unwrap())
        }
    }

    /// A future that returns Pending once, then Ready.
    /// It saves the Waker and the test manually calls wake().
    struct YieldOnce {
        yielded: bool,
        waker: Arc<Mutex<Option<std::task::Waker>>>,
    }

    impl YieldOnce {
        fn new(waker_slot: Arc<Mutex<Option<std::task::Waker>>>) -> Self {
            Self {
                yielded: false,
                waker: waker_slot,
            }
        }
    }

    impl Future for YieldOnce {
        type Output = u32;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
            if self.yielded {
                Poll::Ready(42)
            } else {
                self.yielded = true;
                *self.waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    /// Simplest case: an already-ready future.
    #[test]
    fn block_on_ready_future() {
        let f = Ready(Some(42));
        assert_eq!(block_on(f), 42);
    }

    /// An async block that returns a computed value.
    #[test]
    fn block_on_async_block() {
        assert_eq!(block_on(async { 2 + 3 }), 5);
    }

    /// A future that yields once (returns Pending) before completing.
    /// We wake it from a separate thread to simulate an external event.
    #[test]
    fn block_on_yields_once() {
        let waker_slot: Arc<Mutex<Option<std::task::Waker>>> = Arc::new(Mutex::new(None));
        let waker_slot_clone = Arc::clone(&waker_slot);

        // Spawn a thread that waits until the waker is registered, then fires it.
        let handle = thread::spawn(move || {
            // Spin briefly until the future has registered its waker.
            loop {
                let guard = waker_slot_clone.lock().unwrap();
                if let Some(w) = guard.as_ref() {
                    w.wake_by_ref();
                    break;
                }
            }
        });

        let val = block_on(YieldOnce::new(Arc::clone(&waker_slot)));
        assert_eq!(val, 42);
        handle.join().unwrap();
    }

    /// block_on must work with deeply nested async blocks.
    #[test]
    fn block_on_nested_async() {
        assert_eq!(block_on(async { async { async { 2 + 3 }.await }.await }), 5);
    }

    /// The output type can be non-Copy (e.g. String).
    #[test]
    fn block_on_returns_string() {
        assert_eq!(block_on(async { String::from("test") }), "test".to_string());
    }
}
