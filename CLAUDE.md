# Mini-Tokio — Session Context

## What This Project Is

A from-scratch async runtime built as a learning exercise.
We implement Tokio's core pieces one step at a time, reading Tokio source as reference.

**Tokio source:** `/Users/vova/work/workspace/tokio` (read-only)
**Shared Tokio architecture notes:** `/Users/vova/work/workspace/rust-learning/tokio/CLAUDE.md`

---

## User Context

- Learning Tokio internals from scratch
- Wants to understand low-level concepts deeply before writing code
- Prefers explanations with diagrams and concrete examples before diving in
- Writing code himself — Claude acts as a senior reviewer and guide, not the author
- Code must be idiomatic Rust, testable, with high test coverage

---

## Project Structure

```
mini-tokio/
├── CLAUDE.md               ← this file
├── ARCHITECTURE.md         ← full architecture plan and build roadmap
├── notes.md                ← running log of concepts explained in conversation
├── Cargo.toml
└── src/
    ├── lib.rs
    └── step1/
        ├── mod.rs          ← public API + flow diagram
        ├── waker.rs        ← Step 1a: RawWaker vtable backed by Arc<Thread>
        └── executor.rs     ← Step 1b: block_on (polls future, parks thread)
```

---

## Build Plan

| Step | What | Key concepts | Status |
|------|------|-------------|--------|
| 1a | `Waker` — `RawWakerVTable` backed by `Arc<Thread>` | vtable, Arc::into_raw/from_raw, mem::forget | **in progress** |
| 1b | `block_on` — run one future to completion | pin, Context, thread::park | not started |
| 2  | `spawn` + single-thread scheduler | VecDeque run queue, Arc<Task> | not started |
| 3  | I/O reactor via mio | mio::Poll, ScheduledIo, epoll | not started |
| 4  | Multi-thread work-stealing | crossbeam-deque, inject queue | not started |

---

## Where We Left Off

**Currently implementing:** `src/step1/waker.rs`

The user has the skeleton in place:
- `VTABLE` defined with `RawWakerVTable::new(clone, wake, wake_by_ref, drop_waker)` ✓
- Four function stubs exist but bodies are empty
- `thread_waker()` still has `todo!()`

**Next action:** fill in the four vtable functions and `thread_waker()`.

### What the user understands
- Why Waker uses a manual vtable instead of `dyn Trait` (allocation cost)
- The four vtable functions and their ownership rules
- `Arc::into_raw` / `Arc::from_raw` as the mechanism to go between `Arc` and `*const ()`
- The bug in the current `clone` stub (copies pointer without incrementing refcount)
- `mem::forget` is needed in `clone` and `wake_by_ref` to prevent decrementing refcount

### Key implementation notes explained in session
- `clone`: `from_raw` → `Arc::clone` → `into_raw` the clone → `mem::forget` the original
- `wake`: `from_raw` → `unpark` → let arc drop (decrements refcount)
- `wake_by_ref`: `from_raw` → `unpark` → `mem::forget` (caller still owns)
- `drop_waker`: `from_raw` → let arc drop (no unpark)
- `thread_waker`: `Arc::new(thread)` → `into_raw` → `RawWaker::new` → `Waker::from_raw`

---

## Concepts Covered (see notes.md for full detail)

- Vtable: what it is, C origin, how Rust uses it for `dyn Trait`
- Static vs dynamic dispatch — monomorphization vs fat pointer
- Why `Waker` uses `RawWakerVTable` instead of `Box<dyn Wake>`
- The four vtable functions: clone / wake / wake_by_ref / drop
- `Arc::into_raw` / `Arc::from_raw` / `mem::forget` — manual refcount control
- Who creates the Waker (the executor, from the task pointer)
- How I/O registration works: TcpStream → Registration → ScheduledIo → epoll → wake
- Waker vs I/O Driver — user confused these initially, now clear
