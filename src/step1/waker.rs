// The Waker for block_on.
//
// A Waker is a type-erased handle with four operations: clone, wake, wake_by_ref, drop.
// We encode these as a RawWakerVTable — four function pointers — and store a pointer
// to a Thread handle as the data payload.
//
// Why Thread?
//   thread::park()   — puts the current OS thread to sleep
//   thread::unpark() — wakes a specific Thread (can be called from any thread)
//
// Memory contract (you must uphold this):
//   - `data` is a *const () that was created from Box::into_raw(Box::new(thread))
//     cast to *const ().
//   - clone  → increment refcount  (here: Box::new another clone, or use Arc)
//   - wake   → wake the thread AND drop the data pointer (consumes self)
//   - wake_by_ref → wake the thread WITHOUT dropping (borrows self)
//   - drop   → drop the data pointer without waking
//
// TODO: implement the four vtable functions and `thread_waker()`.
//
// Hint: the simplest correct approach is to wrap Thread in an Arc so that
// clone just does Arc::clone and drop just does Arc::decrement.
// Arc::into_raw / Arc::from_raw let you go between Arc<Thread> and *const ().

use std::mem;
use std::sync::Arc;
use std::task::{RawWaker, RawWakerVTable, Waker};
use std::thread::Thread;

/// Vtable for our thread-unpark waker.
///
/// define VTABLE as a `static RawWakerVTable` with your four functions.
static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_waker);

/// TODO: implement clone
/// Receives *const () pointing to an Arc<Thread>. Must return a new RawWaker
/// with an independently owned clone of the Arc.
unsafe fn clone(data: *const ()) -> RawWaker {
    // let thrd = unsafe { Arc::from_raw(data as *const Thread) };
    // let clone = Arc::clone(&thrd);
    // let data = Arc::into_raw(clone) as *const ();
    // mem::forget(thrd);
    Arc::increment_strong_count(data as *const Thread);
    RawWaker::new(data, &VTABLE)
}

/// TODO: implement wake
/// Receives *const () and consumes it (like Drop + unpark).
unsafe fn wake(data: *const ()) {}

/// TODO: implement wake_by_ref
/// Receives *const () but does NOT consume it — just unparks.
unsafe fn wake_by_ref(data: *const ()) {}

/// TODO: implement drop_waker
/// Receives *const () and drops the Arc without waking.
unsafe fn drop_waker(data: *const ()) {}

/// Build a [`Waker`] that unparks `thread` when called.
///
/// TODO: implement this function.
/// Hint: Box or Arc the Thread, call into_raw(), cast to *const (), build RawWaker.
pub fn thread_waker(thread: Thread) -> Waker {
    todo!("build a RawWaker from the Thread and wrap it in a Waker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Waking must unpark the target thread.
    #[test]
    fn wake_unparks_thread() {
        // TODO: spawn a thread that parks itself, wake it from here, assert it unblocks.
        todo!()
    }

    /// Cloning a waker and waking via the clone must also unpark.
    #[test]
    fn cloned_waker_unparks_thread() {
        todo!()
    }

    /// Dropping a waker without waking must not panic or leak.
    #[test]
    fn drop_does_not_wake() {
        let waker = thread_waker(thread::current());
        drop(waker);
        // If we reach here without crashing, memory was handled correctly.
    }
}
