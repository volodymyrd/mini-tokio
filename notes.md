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
