// Step 1: block_on
//
// Goal: run a single future to completion on the calling thread.
// No scheduler, no queue — just one future and a waker that unparks this thread.
//
// Flow:
//
//   block_on(future)
//       │
//       ▼
//   pin the future on the stack
//   build a Waker backed by the current thread handle
//       │
//       ▼
//   loop {
//       match future.poll(cx) {
//           Poll::Ready(v)  => return v
//           Poll::Pending   => thread::park()   ← OS puts thread to sleep
//       }                            ▲
//   }                                │
//                             waker.wake() calls thread::unpark()
//                             (called by whoever resolves the future)
//
// Key types from std you will use:
//   std::future::Future
//   std::task::{Context, Poll, Waker, RawWaker, RawWakerVTable}
//   std::pin::Pin
//   std::thread::{self, Thread}
//
// Read in Tokio source before you start:
//   tokio/src/runtime/park.rs          ← how Tokio parks/unparks worker threads
//   tokio/src/runtime/task/waker.rs    ← how Tokio builds a Waker from a raw pointer

mod waker;
mod executor;

pub use executor::block_on;
