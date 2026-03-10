use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

struct SimpleWaker;

impl Wake for SimpleWaker {
    fn wake(self: Arc<Self>) {}
}

pub(crate) fn block_on<F: Future>(mut future: F) -> F::Output {
    let mut future = unsafe { Pin::new_unchecked(&mut future) };

    let waker = Waker::from(Arc::new(SimpleWaker));
    let mut context = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
}
