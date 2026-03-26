# Notes: Concepts Behind Mini-Tokio

A running log of concepts covered during the learning process.

---

## Vtables and Dispatch

### What is a vtable?

A **vtable** (virtual dispatch table) is a table of function pointers that describes
how to operate on a specific type, without naming that type at compile time.

Origin: C pattern for polymorphism via `void *data` + struct of function pointers.

```c
struct PlayerVTable {
    void (*play)(void *data);
    void (*stop)(void *data);
};

struct Player {
    void         *data;    // points to format-specific state
    PlayerVTable *vtable;  // points to the right functions for this format
};
```

One `Player` shape, different vtables depending on the concrete format (MP3, WAV, etc.).

---

### Static dispatch — no vtable

When the concrete type is known at compile time, Rust uses **monomorphization**:
one copy of the function is generated per concrete type.

```rust
fn greet<T: Greet>(g: T) {
    g.hello();
}

greet(English);   // compiler emits: greet_English
greet(Spanish);   // compiler emits: greet_Spanish
```

The call target is baked in. No pointer lookup. Zero runtime overhead, larger binary.
**No vtable is created.**

---

### Dynamic dispatch — vtable created

When the concrete type is unknown at compile time, Rust uses `dyn Trait`.
A `&dyn Trait` is a **fat pointer**: two words — data pointer + vtable pointer.

```rust
fn greet(g: &dyn Greet) {
    g.hello();   // resolved at runtime via vtable lookup
}
```

The compiler generates:
- One function body (works for any `Greet`)
- One vtable per concrete type that implements the trait

```
vtable for English: { hello: English::hello, drop: ..., size: ..., align: ... }
vtable for Spanish: { hello: Spanish::hello, drop: ..., size: ..., align: ... }
```

**The vtable only appears when you write `dyn`.**

---

### The rule

| Syntax              | Dispatch | Vtable? | Cost                              |
|---------------------|----------|---------|-----------------------------------|
| `T: Trait`          | static   | no      | zero — resolved at compile time   |
| `impl Trait`        | static   | no      | zero — resolved at compile time   |
| `&dyn Trait`        | dynamic  | yes     | one pointer indirection at runtime|
| `Box<dyn Trait>`    | dynamic  | yes     | one pointer indirection + heap alloc |

---

## Why Waker Uses a Manual Vtable (`RawWakerVTable`)

### The problem with `dyn Trait`

`Waker` needs dynamic dispatch — the executor doesn't know at compile time which
runtime built the waker. So generics won't work.

The naive solution would be:

```rust
trait Wake {
    fn wake(self);
    fn wake_by_ref(&self);
    fn clone_waker(&self) -> Box<dyn Wake>;
}

struct Waker(Box<dyn Wake>);
```

This works, but **every clone allocates**. In a real runtime polling millions of
tasks that's a significant overhead.

### The solution: manual vtable, you control the memory

`std` defines `RawWakerVTable` — an explicit vtable you implement yourself:

```rust
pub struct RawWakerVTable {
    clone:       unsafe fn(*const ()) -> RawWaker,
    wake:        unsafe fn(*const ()),
    wake_by_ref: unsafe fn(*const ()),
    drop:        unsafe fn(*const ()),
}
```

And `RawWaker` is just your data pointer + a reference to that vtable:

```rust
pub struct RawWaker {
    data:   *const (),                   // your data, type-erased
    vtable: &'static RawWakerVTable,     // which functions to call on it
}
```

`Waker` is a safe wrapper around `RawWaker`. Same dynamic dispatch as `dyn Trait`,
but you decide how memory is allocated and freed — no forced heap allocation on clone.

---

## The Four Vtable Functions

Each function receives `*const ()` — a raw, type-erased pointer to your data.
You cast it back to your concrete type inside each function.

| Function      | Ownership of `data` | Must do                                  |
|---------------|---------------------|------------------------------------------|
| `clone`       | borrows             | produce a new independent `RawWaker`     |
| `wake`        | consumes            | wake + free data                         |
| `wake_by_ref` | borrows             | wake, do NOT free data                   |
| `drop`        | consumes            | free data, do NOT wake                   |

---

## Why Exactly These Four Functions?

A `Waker` is a value type passed across threads. Think of it as a ref-counted
smart pointer to a callback. The four vtable functions are the **minimum complete
set** for managing an owned, cloneable, cross-thread handle:

| Function      | Why it exists                                                                 |
|---------------|-------------------------------------------------------------------------------|
| `clone`       | Multiple owners need independent copies (future stores one, scheduler keeps one) |
| `wake`        | The whole point — signal "poll me again". Consumes self (wake + drop in one step) |
| `wake_by_ref` | Wake *without* giving up your copy (e.g. a timer that fires repeatedly). Avoids clone+wake |
| `drop`        | You allocated something — must free it when the waker is no longer needed     |

These map directly to `Arc` operations:

```
clone        → Arc::clone          (refcount++)
wake         → do_something + drop (consume: refcount--)
wake_by_ref  → do_something        (borrow: refcount unchanged)
drop         → Arc::drop           (refcount--)
```

**Why no `new`?** Construction is type-specific — each executor builds wakers
differently (our `thread_waker()`, Tokio's task-pointer waker, etc.).

**Why no `wake_by_mut`?** Wakers must be `Send + Sync` (shared across threads),
so exclusive mutable access can't be assumed.

---

## `*const ()` — Rust's `void*`

`RawWaker` stores data as `*const ()` — a type-erased raw pointer, equivalent
to C's `void*`. The vtable functions cast it back to the concrete type:

| C                        | Rust                          |
|--------------------------|-------------------------------|
| `void *data`             | `data: *const ()`             |
| `(Thread*)data`          | `data as *const Thread`       |

`()` is zero-sized, so the pointer carries no type information — exactly like
`void`. The vtable functions know the real type; the generic `Waker` machinery
doesn't need to.

---

## `Arc::increment_strong_count` — Simpler Clone

The manual clone pattern (reconstruct → clone → into_raw → forget) can be
replaced with a single call:

```rust
// 4 lines:
let arc = Arc::from_raw(data as *const Thread);  // reconstruct
let clone = Arc::clone(&arc);                     // refcount++
let ptr = Arc::into_raw(clone) as *const ();      // new raw pointer
mem::forget(arc);                                  // don't decrement original

// 1 line (same effect):
Arc::increment_strong_count(data as *const Thread); // refcount++, done
```

Added in Rust 1.51 for exactly this pattern — bumps the refcount directly from
a raw pointer without the reconstruct-forget dance.

---

## Our Waker for `block_on`: `Arc<Thread>`

For `block_on` we just need to unpark a thread.
- `Thread` is a handle to an OS thread; `thread.unpark()` wakes it.
- `Arc<Thread>` gives shared ownership across waker clones.
- `Arc::into_raw` → `*const ()` to store in `RawWaker`.
- `Arc::from_raw` inside each vtable function to reconstruct ownership.

```
RawWaker {
    data ──────────────────► Arc<Thread>   (refcount-managed Thread handle)
    vtable ─────────────────► RawWakerVTable {
                                clone:       Arc::clone, leak the clone
                                wake:        unpark + drop Arc
                                wake_by_ref: unpark, mem::forget (don't drop)
                                drop:        drop Arc (no unpark)
                              }
}
```

### Memory contract step by step

```
thread_waker(t)
    Arc::new(t)                          refcount = 1
    Arc::into_raw() as *const ()         we are responsible for this pointer now

clone(data)
    Arc::from_raw(data)                  reconstruct Arc (refcount still 1)
    Arc::clone(&arc)                     refcount = 2
    Arc::into_raw(clone) as *const ()    new waker owns this pointer
    mem::forget(arc)                     don't drop original (refcount stays 2)
    RawWaker { clone_ptr, &VTABLE }

wake(data)
    Arc::from_raw(data)                  take ownership back
    arc.unpark()
    drop(arc)                            refcount--, free if last

wake_by_ref(data)
    Arc::from_raw(data)                  reconstruct temporarily
    arc.unpark()
    mem::forget(arc)                     caller still owns it, don't decrement

drop(data)
    Arc::from_raw(data)                  take ownership back
    drop(arc)                            refcount--, free if last, no unpark
```

---

## Why `poll` Needs a `Context` (and therefore a `Waker`)

The dependency chain — nothing works without the bottom layer:

```
RawWakerVTable   (4 fn pointers you implement)
      │
      ▼
RawWaker         (vtable + *const () data pointer)
      │
      ▼
Waker            (safe wrapper — what futures store and call wake() on)
      │
      ▼
Context          (thin wrapper around &Waker, passed into poll)
      │
      ▼
future.poll(&mut cx)
```

A future returns `Poll::Pending` when it can't make progress. Before doing so it
saves `cx.waker().clone()` somewhere. When the external event arrives, whoever
handles it calls `waker.wake()` — signalling the executor to poll the task again.

```
executor              future                  external event
    │                    │                         │
    ├──poll(cx)─────────►│                         │
    │                    │ saves cx.waker()         │
    │◄──Pending──────────│                         │
    │  parks thread      │      data arrives        │
    │                    │◄────────────────────────-│
    │                    │  waker.wake()            │
    │◄────────────────────────────────────────────-─│
    │  unparked!         │                         │
    ├──poll(cx)─────────►│                         │
    │◄──Ready(val)───────│                         │
```

Waker is the connective tissue between futures and the executor.
Without it a future has no way to say "come poll me again."

---

## Who Creates a Waker, and How "Registering Interest" Works

### Who creates the Waker?

**The executor creates it** — right before calling `poll` on a task.

In Tokio the waker's data pointer IS the task pointer itself
(`tokio/src/runtime/task/waker.rs:28`):

```rust
let waker = Waker::from_raw(raw_waker(header));
//                                     ^^^^^^
//                          pointer to the Task's own allocation
```

When `wake()` is called, Tokio knows exactly which task to re-schedule because
the task is its own waker data. No lookup table needed.

```
executor owns Task
    │
    ├─ builds Waker from pointer to Task
    ├─ wraps it in Context
    └─ calls future.poll(&mut cx)
                  │
                  future now holds cx.waker().clone()
                  and can hand it to anyone
```

---

### Three players in I/O registration

```
TcpStream          Registration          ScheduledIo
(what you use)     (the bridge)          (lives in the driver, one per fd)
```

**`ScheduledIo`** is the key struct — it lives inside the I/O driver and holds:

```rust
ScheduledIo {
    readiness: AtomicUsize,    // is the fd readable/writable yet?
    waiters:   Mutex<Waiters>, // the stored Waker(s)
}
```

---

### Step-by-step: from `.await` to wake

**Step 1 — TcpStream is created**

`TcpStream::connect()` calls (`registration.rs:78`):

```rust
handle.driver().io().add_source(io, interest)
```

This does two things simultaneously:
- tells mio (→ epoll) to watch this socket fd
- allocates a `ScheduledIo` for this fd and stores it in the driver's registry

**Step 2 — you `.await` a read**

Inside `TcpStream::poll_read()` the socket isn't ready yet, so:

```rust
// roughly what happens inside registration.poll_read_ready(cx)
scheduled_io.waiters.lock().reader = Some(cx.waker().clone());
return Poll::Pending;
```

The Waker is now **sitting inside `ScheduledIo`**, waiting for the OS event.

**Step 3 — epoll fires**

When the run queue drains, the scheduler calls the I/O driver
(`driver.rs:188`):

```rust
self.poll.poll(events, max_wait)  // blocks until OS says something is ready
```

When epoll wakes it (`driver.rs:209-219`):

```rust
let io: &ScheduledIo = unsafe { &*ptr };  // look up the ScheduledIo for this fd
io.set_readiness(Tick::Set, |curr| curr | ready);
io.wake(ready);                           // ← calls the stored Waker
```

**Step 4 — task is re-scheduled**

`io.wake()` pulls the stored Waker from `ScheduledIo.waiters` and calls
`waker.wake()`. In Tokio's waker vtable (`task/waker.rs:101-102`):

```rust
let raw = RawTask::from_raw(ptr);  // ptr is the task itself
raw.wake_by_val();                 // push task back onto the run queue
```

The task lands in the scheduler's queue. On the next iteration the worker
polls it again — now the socket has data → `Poll::Ready(bytes)`.

---

### The full picture, end to end

```
Your code           Tokio internals                  OS
─────────           ───────────────                  ──

TcpStream::read().await
                    executor creates Waker (→ task ptr)
                    polls TcpStream future
                    future stores Waker in ScheduledIo
                    returns Poll::Pending
                    executor parks thread
                                                     epoll_wait() blocks...
                                                     ...socket gets data
                                                     epoll_wait() returns
                    driver finds ScheduledIo for fd
                    calls io.wake()
                    waker.wake() → task pushed to run queue
                    executor unparks, polls task again
                    TcpStream reads socket → Poll::Ready(bytes)
read() returns bytes to your code
```

---

### Key insight

The Waker is the **handoff object** between two worlds:

| World | Role |
|---|---|
| Async world (futures, executor) | creates the Waker, passes it into `poll` |
| Event world (I/O driver, timers, channels) | stores the Waker, calls `wake()` when ready |

Neither world needs to know about the other. They communicate only through
this one small, type-erased object.

For Step 1 of mini-tokio there is no I/O driver — the waker just calls
`thread::unpark()`. But the contract is identical: someone stores the waker,
an event happens, `wake()` is called.

---

## std vs Executor: Division of Responsibilities

### std provides the interface (no runtime)

Everything needed to **define** async code and the waker contract:

| Provided by std          | Purpose                                      |
|--------------------------|----------------------------------------------|
| `Future` trait + `Poll`  | What to poll, what it returns                |
| `Waker` + `RawWaker`    | Type-erased callback handle ("poll me again")|
| `RawWakerVTable`         | 4 function pointers: clone/wake/wake_by_ref/drop |
| `Context`                | Carries the Waker into `poll()`              |
| `Pin`                    | Safety for self-referential futures           |

std gives you all the **interface** — but no executor, no scheduler, no I/O loop.

### The executor provides the implementation

What Tokio (or any runtime) builds on top of std:

| Executor decides         | Example (Tokio)                              |
|--------------------------|----------------------------------------------|
| When to poll futures     | Worker loop drains run queue                 |
| Where to store tasks     | Per-worker deque + global inject queue        |
| How to schedule threads  | Work-stealing across N workers               |
| How to integrate OS I/O  | mio → epoll/kqueue, ScheduledIo per fd       |
| How to handle timers     | Hierarchical timer wheel                     |
| What `data` in Waker is  | Pointer to Task Header                       |

**Any program can build its own executor** using only std primitives.
That's exactly what mini-tokio does.

---

## Why Tokio Can't Use a Concrete Waker Type

Two reasons the vtable indirection is unavoidable:

### 1. `Waker` is defined in std, not in Tokio

`Future::poll` signature is fixed in the standard library:

```rust
fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T>
```

`Context` holds a `std::task::Waker`. Every future in the ecosystem — Tokio's,
third-party crates, your own — receives this type. std can't hardcode any
executor's task type.

### 2. Even within Tokio, the task type is generic

`Task<T: Future>` has a different concrete type per spawned future. The `Header`
is the non-generic prefix that all tasks share. The vtable erases `T` so the
scheduler can handle all tasks uniformly through `*const Header`.

**The vtable is the price of one universal `Future` trait across all executors.**

---

## What the Waker's `data` Pointer Holds (Across Executors)

The `data: *const ()` in `RawWaker` is executor-specific:

| Executor / context         | `data` points to          | `wake()` does                    |
|----------------------------|---------------------------|----------------------------------|
| mini-tokio step 1          | `Arc<Thread>`             | `thread.unpark()`                |
| mini-tokio step 2+         | task struct pointer       | pushes task onto run queue       |
| Real Tokio                 | `Task` Header (`NonNull<Header>`) | `RawTask::wake_by_val()` — enqueues task |
| async-std                  | task pointer              | schedules task for re-poll       |
| smol                       | `Runnable` handle         | enqueues into executor           |
| embassy (embedded, no OS)  | task pointer              | marks task ready, no threads     |
| Trivial executor           | `AtomicBool` pointer      | sets flag to `true`              |

The pattern is always: `data` = "who to wake", vtable = "how to wake them."
The Waker doesn't know who gave it or why it will be woken.

---

## Step 1b: `block_on` — Running a Future to Completion

### What `block_on` does

The simplest possible executor: takes **one** future, polls it in a loop on the
calling thread, and parks the thread between polls. No task queue, no spawning,
no I/O driver.

```
block_on(future)
    │
    ├─ pin the future (it must not move in memory)
    ├─ build a Waker that calls thread::unpark()
    ├─ wrap the Waker in a Context
    │
    └─ loop {
           poll the future with &mut cx
           Ready(val)  → return val
           Pending     → thread::park()  ← OS sleeps this thread
       }                      ▲
                        waker.wake() unparks it
```

---

### Pinning: why and how

`Future::poll` requires `Pin<&mut Self>`:

```rust
fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T>
```

**Why?** Async blocks compile into state machines that may contain self-references
(a borrow pointing into the same struct). If the struct moves in memory, those
internal pointers become dangling. `Pin` is a compile-time guarantee that the
future **will not be moved** after pinning.

**How to pin for `block_on`:**

Option A — stack pinning with `std::pin::pin!` (preferred, no heap allocation):

```rust
let mut future = std::pin::pin!(future);
// future is now Pin<&mut F>, pinned to the stack frame
// use future.as_mut() to get Pin<&mut F> for each poll call
```

Option B — heap pinning with `Box::pin` (allocates):

```rust
let mut future = Box::pin(future);
// future is Pin<Box<F>>, pinned on the heap
// use future.as_mut() for polling
```

For `block_on` stack pinning is ideal — the future lives for the duration of
the function call and never needs to be sent to another thread.

**Key rule:** after pinning, you can only access the future through
`Pin<&mut F>`. Use `.as_mut()` each time you poll (it reborrows without moving).

---

### Context: the thin wrapper around Waker

```rust
// std defines:
pub struct Context<'a> {
    waker: &'a Waker,
    // (+ _marker for lifetime variance)
}
```

Create it with:

```rust
let waker = thread_waker(thread::current());
let cx = &mut Context::from_waker(&waker);
```

The future receives `cx` in its `poll` method. Inside, it calls
`cx.waker().clone()` to save the waker for later use.

**Why `&mut Context` and not just `&Waker`?** Forward compatibility — `Context`
can be extended with additional fields (e.g., task-local storage) without
breaking the `Future` trait signature.

---

### The poll loop and spurious wakeups

```rust
loop {
    match future.as_mut().poll(cx) {
        Poll::Ready(val) => return val,
        Poll::Pending    => thread::park(),
    }
}
```

**Why loop and not just poll once?** Two reasons:

1. **Spurious wakeups** — `thread::park()` may return even if nobody called
   `unpark()`. The OS or runtime can wake the thread for internal reasons.
   You must re-poll to check if the future is actually ready.

2. **Multiple pending states** — a future may need several poll cycles to
   complete (e.g., it returns `Pending` once to register a waker, then
   `Ready` on the next poll after the event fires).

**Why `as_mut()`?** `poll` takes `self: Pin<&mut Self>` which consumes the
`Pin<&mut F>`. Calling `.as_mut()` reborrows it — producing a new
`Pin<&mut F>` without moving the future. Without this, the compiler would
reject the second poll because the pin was consumed.

---

### Putting it all together — the full `block_on`

```
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);          // 1. pin on stack
    let waker = thread_waker(thread::current()); // 2. build waker
    let cx = &mut Context::from_waker(&waker);   // 3. build context
    loop {                                        // 4. poll loop
        match future.as_mut().poll(cx) {
            Poll::Ready(val) => return val,
            Poll::Pending    => thread::park(),
        }
    }
}
```

Five lines. Every line maps to a concept:

| Line | Concept |
|------|---------|
| `pin!(future)` | Self-referential futures must not move after first poll |
| `thread_waker(thread::current())` | Our waker vtable from Step 1a — wake = unpark |
| `Context::from_waker(&waker)` | The `poll` API requires a Context, not a bare Waker |
| `future.as_mut().poll(cx)` | Drive the state machine forward one step |
| `thread::park()` | Sleep until the waker fires — don't burn CPU spinning |

---

### How it connects to what Tokio does

| Aspect | mini-tokio `block_on` | Tokio `block_on` |
|--------|----------------------|-------------------|
| Number of futures | 1 | 1 (but can `spawn` more inside) |
| Waker data | `Arc<Thread>` | pointer to `Task` header |
| What `wake()` does | `thread::unpark()` | push task to run queue + unpark worker |
| Parking | `thread::park()` | `driver.park()` (polls I/O + timers while parked) |
| Pinning | `pin!` on stack | `Box::pin` in task allocation |

The core loop is the same: poll → pending → park → wake → poll → ready → done.
Tokio's version just does more work while "parked" (I/O polling, timer ticks).

---

## Step 2: Spawn — Multi-Task Single-Threaded Scheduler

### The problem

`block_on` runs **one** future. Real programs need hundreds of concurrent tasks
(connections, timers, background jobs) on a single thread. We need:

1. A way to **submit** new futures (`spawn`)
2. A **queue** of tasks ready to be polled
3. A **scheduler loop** that drains the queue
4. A **waker** that re-enqueues a task when it's ready again

---

### The big shift from Step 1

| | Step 1 (`block_on`) | Step 2 (`spawn`) |
|---|---|---|
| Futures | 1, concrete type `F` | Many, different types |
| Storage | Stack-pinned | Heap-allocated (`Box::pin`) |
| Waker action | `thread::unpark()` | push task back onto run queue |
| Type | Generic `F: Future` | Type-erased (all tasks are the same type in the queue) |
| Completion | Returns `F::Output` | Fire-and-forget (output is discarded or collected via `JoinHandle`) |

---

### Type erasure: why and how

`spawn` accepts any `F: Future<Output = ()>`. But the run queue needs a single
concrete type. Two options:

**Option A — `dyn Future` (trait object):**

```rust
type Task = Pin<Box<dyn Future<Output = ()>>>;
// Queue is VecDeque<Task>
```

Simple, but the waker needs to push the **same task** back onto the queue.
A `Pin<Box<dyn Future>>` doesn't know which queue position it came from.

**Option B — `Arc`-based task with self-referencing waker (what Tokio does):**

```rust
struct Task {
    future: Mutex<Pin<Box<dyn Future<Output = ()>>>>,
    queue:  /* reference to the run queue */,
}
// Queue is VecDeque<Arc<Task>>
```

The waker holds an `Arc<Task>` — when `wake()` is called, it clones the Arc
and pushes it back onto the queue. The task knows how to re-enqueue itself.

---

### Architecture

```
spawn(future)
    │
    ├─ Box::pin(future)              heap-pin the future (type-erased)
    ├─ wrap in Arc<Task>             shared ownership: queue + waker both hold it
    └─ push Arc<Task> onto queue     ready to be polled

scheduler loop (inside block_on or run)
    │
    └─ loop {
           while let Some(task) = queue.pop_front() {
               build Waker from Arc<Task>
               poll task.future with that Waker
               Ready  → task is done, Arc drops
               Pending → waker will re-enqueue when event fires
           }
           if queue is empty → park thread (wait for wake)
       }

waker.wake()
    │
    └─ push Arc<Task> back onto queue
       unpark the scheduler thread
```

---

### The Task struct

```rust
struct Task {
    future: Mutex<Pin<Box<dyn Future<Output = ()> + Send>>>,
    sender: mpsc::Sender<Arc<Task>>,   // handle to re-enqueue ourselves
}
```

**Why `Mutex`?** The waker might call `wake()` from another thread. Even though
our scheduler is single-threaded, the contract says wakers must be `Send + Sync`.
The `Mutex` makes `Task` safe to share. In practice, only one thread ever locks it.

**Why `Pin<Box<dyn Future>>`?** Two reasons:
- `Box` puts it on the heap (required — tasks outlive the `spawn` call)
- `Pin` guarantees the future won't move (required by `poll`)
- `dyn Future` erases the concrete type (required — queue holds mixed futures)

**Why `mpsc::Sender`?** Simple channel-based queue. `Sender` is `Clone + Send`,
so the waker can hold a copy and push tasks from anywhere. The scheduler holds
the `Receiver` end and pops tasks to poll them.

---

### The Waker for Step 2

Step 1's waker called `thread::unpark()`. Step 2's waker must:
1. Push `Arc<Task>` back onto the run queue
2. Unpark the scheduler thread (so it wakes up to poll)

```
RawWaker {
    data ──► Arc<Task>
    vtable ──► RawWakerVTable {
        clone:       Arc::clone the Task
        wake:        sender.send(arc) + thread::unpark  (consumes)
        wake_by_ref: sender.send(arc.clone()) + thread::unpark  (borrows)
        drop:        drop the Arc
    }
}
```

Or — simpler — use the `std::task::Wake` trait (stabilized in Rust 1.51):

```rust
impl Wake for Task {
    fn wake(self: Arc<Self>) {
        self.sender.send(self.clone()).unwrap();
        // thread::unpark handled by scheduler
    }
}
```

`Arc<Task>` automatically becomes a `Waker` via `Waker::from(arc)`. No manual
vtable needed! This is a major simplification over Step 1.

---

### Queue options

| Option | Type | Pros | Cons |
|--------|------|------|------|
| `VecDeque<Arc<Task>>` + `Mutex` | push/pop manually | Simple, matches Tokio's `current_thread` | Need mutex for cross-thread wake |
| `mpsc::channel` | `Sender`/`Receiver` | Thread-safe by default, waker just sends | Allocation per send |

For learning, `mpsc::channel` is easiest — the `Sender` is what the waker holds,
the `Receiver` is what the scheduler drains.

---

### `spawn` function

```rust
fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let task = Arc::new(Task {
        future: Mutex::new(Box::pin(future)),
        sender: SENDER.clone(),  // global or passed via context
    });
    SENDER.send(task).unwrap();
}
```

**`Send + 'static` bounds:** The future might be polled after `spawn` returns
(hence `'static` — no borrowed data). `Send` because the waker might touch it
from another thread (even in a single-threaded runtime, the contract requires it).

---

### The scheduler loop

```rust
fn run(receiver: mpsc::Receiver<Arc<Task>>) {
    while let Ok(task) = receiver.recv() {   // blocks when queue is empty
        let waker = Waker::from(task.clone());
        let mut cx = Context::from_waker(&waker);
        let mut future = task.future.lock().unwrap();
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(()) => {}           // task done, drop it
            Poll::Pending   => {}           // waker will re-enqueue
        }
    }
}
```

**Key difference from `block_on`:** we don't loop on one future. We pop tasks,
poll each once, and move on. If a task is `Pending`, it's the **waker's job**
to re-enqueue it. If the queue is empty, `recv()` blocks the thread.

---

### How this maps to Tokio's `current_thread` scheduler

| mini-tokio Step 2 | Tokio current_thread |
|---|---|
| `Arc<Task>` with `Mutex<Pin<Box<dyn Future>>>` | `Task<T>` with `Header` + `Core<T>` (raw pointer, no mutex on hot path) |
| `mpsc::channel` | `VecDeque` run queue + inject queue (no channel overhead) |
| `Waker::from(arc)` using `Wake` trait | Manual `RawWakerVTable` pointing to task header |
| `receiver.recv()` blocks | `driver.park()` — blocks on I/O + timers while waiting |
| `spawn` sends to channel | `spawn` pushes to run queue directly |

We trade performance for simplicity. Tokio avoids `Mutex` and channels on the
hot path, but the architecture is the same.

---

### Files to create

```
src/step2/
├── mod.rs          — public API: MiniTokio struct, spawn(), run()
├── task.rs         — Task struct, Wake impl
└── tests.rs or inline #[cfg(test)] — scheduler tests
```

---

### What to test

1. **Spawn a ready future** — spawn one, run scheduler, verify it completes
2. **Spawn multiple futures** — spawn several, all complete
3. **Yield and re-schedule** — a future returns Pending once, waker re-enqueues it
4. **Spawn from within a task** — a running task spawns another task (nested spawn)
5. **Ordering** — tasks run in FIFO order (first spawned, first polled)
