# Mini-Tokio: Learning Tokio Internals from Scratch

This project is a learning exercise — we build a minimal async runtime step by step,
mirroring the real Tokio architecture. We read Tokio source as a reference but write
everything ourselves.

**Reference repo:** `/Users/vova/work/workspace/tokio`

---

## How Tokio is structured (the real thing)

```
┌─────────────────────────────────────────────┐
│                   Runtime                    │  ← entry point, owns everything
│  ┌──────────────┐    ┌────────────────────┐  │
│  │  Scheduler   │    │   Driver (I/O)     │  │
│  │              │    │   (mio/epoll)      │  │
│  │  ┌────────┐  │    └────────────────────┘  │
│  │  │ Task   │  │    ┌────────────────────┐  │
│  │  │ Task   │  │    │   Time Driver      │  │
│  │  │ Task   │  │    └────────────────────┘  │
│  │  └────────┘  │    ┌────────────────────┐  │
│  └──────────────┘    │  Blocking Pool     │  │
└─────────────────────────────────────────────┘
```

### The 5 layers

#### 1. `Future` + `Poll` + `Waker` (from `std`)

The foundation. Tokio doesn't own this — it comes from `std::task`.

- A `Future` is a state machine. Each `.await` point is a state transition.
- `Poll::Ready(val)` — done, here's the value.
- `Poll::Pending` — not ready yet, I'll call `wake()` when I am.
- A `Waker` is essentially a fat pointer: `(data: *const (), vtable: &WakerVTable)`.
  The vtable has four function pointers: `clone`, `wake`, `wake_by_ref`, `drop`.
  Calling `wake()` tells the runtime "re-schedule this task".

**Reference in Tokio source:**
- `tokio/src/runtime/task/waker.rs` — how Tokio constructs a `Waker` from a `Task`

#### 2. `Task` — the schedulable unit

A task is more than `Box<dyn Future>`. It is:
- The pinned, heap-allocated future
- Its current execution state (idle / running / notified / complete)
- A reference back to the scheduler so the `Waker` can re-enqueue it

Tokio uses a type-erased `RawTask` with a custom vtable covering:
`poll`, `schedule`, `dealloc`, `read_output`.
This avoids double-boxing and lets the scheduler hold tasks without knowing the
concrete future type.

**Reference in Tokio source:**
- `tokio/src/runtime/task/core.rs` — the task allocation layout
- `tokio/src/runtime/task/raw.rs` — the type-erased vtable
- `tokio/src/runtime/task/harness.rs` — the poll loop and state transitions
- `tokio/src/runtime/task/state.rs` — atomic state machine (idle/running/notified/…)

#### 3. `Scheduler` — two flavors

**CurrentThread** (single-threaded)
- All tasks run on the calling thread.
- Run queue is a simple `VecDeque` of ready tasks.
- When a task is woken, it is pushed to the back of the queue.
- The driver is polled (for I/O events) when the queue drains.

**MultiThread** (work-stealing thread pool)
- Each worker thread has its own local run queue (a bounded lock-free deque).
- A shared global **inject queue** receives tasks spawned from outside any worker.
- Idle workers steal tasks from the tail of other workers' local queues.
- A parked worker is unparked via a condvar/futex when new work appears.

**Reference in Tokio source:**
- `tokio/src/runtime/scheduler/current_thread/mod.rs`
- `tokio/src/runtime/scheduler/multi_thread/worker.rs`
- `tokio/src/runtime/scheduler/multi_thread/queue.rs` — local work-stealing deque
- `tokio/src/runtime/scheduler/inject/shared.rs` — global inject queue

#### 4. `Driver` — the I/O reactor

Wraps `mio`, which wraps OS primitives (epoll on Linux, kqueue on macOS, IOCP on Windows).

Flow:
1. A task calls `.await` on an async I/O operation (e.g. `TcpStream::read`).
2. The I/O future registers interest with the driver (`mio::Registry::register`).
3. The future returns `Poll::Pending`. The task is parked.
4. When the queue is empty, the scheduler calls `mio::Poll::poll()` — this blocks
   the thread until the OS signals readiness.
5. The driver wakes all tasks registered for the ready events.

**Reference in Tokio source:**
- `tokio/src/runtime/io/driver.rs` — the mio event loop
- `tokio/src/runtime/io/registration.rs` — per-resource interest registration
- `tokio/src/runtime/io/scheduled_io.rs` — per-resource waker storage
- `tokio/src/runtime/park.rs` — thread park/unpark around the driver poll

#### 5. `Time Driver`

A **hierarchical timer wheel**. Think of it as a clock face with multiple hands at
different resolutions (ms, seconds, minutes, …).

- `tokio::time::sleep(d)` allocates a timer entry and inserts it into the wheel.
- Each time the driver ticks, it advances the wheel and collects expired entries.
- Expired entries have their stored `Waker` called, waking the sleeping task.

**Reference in Tokio source:**
- `tokio/src/runtime/time/` — timer wheel implementation
- `tokio/src/runtime/time/entry.rs` — a single timer entry
- `tokio/src/runtime/time_alt/` — alternative timer implementation (unstable)

#### 6. `Blocking Pool`

`tokio::task::spawn_blocking(|| { ... })` offloads blocking work to a dedicated
thread pool so it never stalls the async workers. This pool grows on demand and
shrinks when threads idle for too long.

**Reference in Tokio source:**
- `tokio/src/runtime/blocking/pool.rs`

---

## What we build — step by step

Each step is a standalone, runnable program. We add one layer at a time.

### Step 1: `block_on`

Run a single future to completion on the calling thread.

**Concepts covered:** `RawWaker`, `RawWakerVTable`, pinning, `thread::park` / `unpark`.

**Key insight:** The waker just needs to unpark the blocked thread. No queue, no
scheduler — just: poll → if Pending, park → woken by waker → poll again.

```
Future ──poll──► Ready  →  done
              └► Pending → park thread
                           ▲
                    waker.wake() unparks it
```

**Read first:** `tokio/src/runtime/park.rs`

---

### Step 2: `spawn` + single-threaded executor

Multiple concurrent tasks on one thread. This is the `CurrentThread` runtime.

**Concepts covered:** run queue (`VecDeque`), `Arc`-shared scheduler, waker that
re-enqueues the task by index or pointer.

```
run_queue: [Task A, Task B, Task C]
     │
     ▼
 pop front → poll
   Ready  → drop task
   Pending → task sits idle until waker re-pushes it
```

**Read first:** `tokio/src/runtime/scheduler/current_thread/mod.rs`

---

### Step 3: async I/O (mini reactor)

Add `mio`. Register TCP sockets. Park the thread when the queue is empty, wake
on I/O events.

**Concepts covered:** `mio::Poll`, `mio::Registry`, interest tokens, waker storage
per I/O resource, the event loop.

```
loop {
    while let Some(task) = run_queue.pop() { poll(task); }
    mio::poll(&mut events, timeout);   // ← blocks until I/O ready
    for event in events { wake_registered_task(event.token()); }
}
```

**Read first:** `tokio/src/runtime/io/driver.rs`

---

### Step 4 (stretch): multi-threaded work-stealing

Spawn N worker threads. Each has a local queue. Add a shared inject queue.
Implement task stealing.

**Concepts covered:** `crossbeam-deque`, `Arc<Injector>`, idle worker parking,
work stealing from siblings.

```
Worker 0: [T1, T2, T3] ◄── steals ── Worker 1: []
                ▲
         Inject queue (new spawns from anywhere)
```

**Read first:** `tokio/src/runtime/scheduler/multi_thread/worker.rs`

---

## Key types cheat-sheet

| Concept | Tokio type | Our mini version |
|---|---|---|
| Async task | `task::RawTask` | `Task` struct with `Box<dyn Future>` |
| Task state | `task::State` (atomic) | simple `enum` or `AtomicU8` |
| Waker | built from `task::waker` vtable | `RawWaker` + `thread::unpark` |
| Run queue (1 thread) | `VecDeque` in `CurrentThread` | `VecDeque<Arc<Task>>` |
| Run queue (N threads) | `crossbeam_deque::Worker` | `crossbeam-deque` |
| Inject queue | `scheduler::inject::Shared` | `crossbeam_deque::Injector` |
| I/O driver | `mio::Poll` wrapper | `mio::Poll` directly |
| Timer | hierarchical wheel | `BinaryHeap` of `(Instant, Waker)` |
| Blocking pool | `blocking::Pool` | `std::thread::spawn` per task |

---

## Rust concepts you'll exercise

- `std::future::Future` and manual `poll` implementations
- `std::task::{Waker, RawWaker, RawWakerVTable, Context}`
- `std::pin::Pin` and why futures must be pinned before polling
- `Arc` + interior mutability (`Mutex`, `AtomicUsize`) for shared state
- Unsafe Rust: building a `RawWaker`, raw pointer aliasing rules
- `thread::park` / `thread::unpark` for parking/waking threads
- `mio` for non-blocking I/O (step 3+)
- `crossbeam-deque` for work-stealing queues (step 4)
