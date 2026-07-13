# First draft with Gemini

To implement this isolated-actors architecture with a centralized Mediator and shared-memory buffers in Rust, you have excellent options both in the standard library (`std`) and in the async crate ecosystem (such as the `tokio` ecosystem).

---

## 1. Tools in the Core (`std`) and Main Ecosystem

For the **messaging and isolation** part:

* **`tokio::sync::mpsc` and `oneshot`**: As we saw, these are the ideal async channels for sending messages from the Mediator to the processes and for the async return (*Response Channels*).
* **`std::sync::mpsc`**: Rust's native channel. It's synchronous and excellent if you're working with native OS threads (`std::thread`) instead of async tasks.

For the **shared memory (Shared Memory Buffers)** part:
If the processes need to access common data buffers extremely fast, Rust offers safe primitives for sharing memory without duplicating data:

* **`std::sync::Arc<T>` (Atomic Reference Counted):** Lets multiple processes (threads/tasks) hold a read pointer to the same block of memory. The data sits in an immutable, safe memory region.
* **`tokio::sync::RwLock<T>` or `std::sync::RwLock<T>` (Read-Write Lock):** Ideal for buffers. Lets **unlimited processes read** the buffer at the same time, but if the Orchestrator needs to update the buffer, it gets exclusive write access, temporarily blocking reads.

---

## 2. How to Integrate Shared Memory into the Architecture

Instead of passing large volumes of data (like a file or a heavy telemetry payload) inside the Mediator's messages, you put that data in a buffer protected by an `Arc<RwLock<Buffer>>`.

In the Mediator's messages, you only send the **ID or metadata** of what needs to be read. The isolated process goes to the buffer and fetches the data on demand.

### Practical Example in Rust (Tokio + Arc + RwLock)

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use std::collections::HashMap;

// 1. The Shared Memory Buffer
#[derive(Default)]
pub struct SharedBuffer {
    // Simulating a cache of packets or payloads indexed by ID
    pub storage: HashMap<u32, Vec<u8>>,
}

// 2. The Mediator's message is now lightweight: it only carries the data's reference (ID)
#[derive(Debug)]
pub struct Envelope {
    pub data_id: u32,
    pub tx_respond: oneshot::Sender<String>,
}

// 3. The Isolated Process that fetches data from the Buffer
pub struct Worker {
    rx: mpsc::Receiver<Envelope>,
    // Each worker gets a copy of the Arc pointer to access the same memory
    shared_buffer: Arc<RwLock<SharedBuffer>>,
}

impl Worker {
    pub fn new(rx: mpsc::Receiver<Envelope>, shared_buffer: Arc<RwLock<SharedBuffer>>) -> Self {
        Self { rx, shared_buffer }
    }

    pub async fn run(mut self) {
        while let Some(envelope) = self.rx.recv().await {
            // Acquire the Read Lock (multiple workers can read at the same time)
            let buffer_guard = self.shared_buffer.read().await;

            // Fetch the buffered data extremely fast (in-memory)
            let response = if let Some(raw_bytes) = buffer_guard.storage.get(&envelope.data_id) {
                format!("Data processed successfully. Size: {} bytes", raw_bytes.len())
            } else {
                "Error: data not found in the shared buffer".to_string()
            };

            // Explicit drop of the guard to release the read as soon as possible (optional, happens at end of scope anyway)
            drop(buffer_guard);

            let _ = envelope.tx_respond.send(response);
        }
    }
}

#[tokio::main]
async fn main() {
    // Creating the buffer on the heap and wrapping it in an Arc for distributed reading
    let shared_buffer = Arc::new(RwLock::new(SharedBuffer::default()));

    // --- Write simulation (e.g. the Orchestrator or input Driver populating the buffer) ---
    {
        let mut write_guard = shared_buffer.write().await;
        write_guard.storage.insert(1001, vec![0xDE, 0xAD, 0xBE, 0xEF]); // Raw buffered data
        println!("[Buffer] Data populated into shared memory.");
    } // The write lock is released here automatically

    // Setting up the Worker
    let (tx_worker, rx_worker) = mpsc::channel::<Envelope>(10);
    // Cloning the Arc only increments the reference count, it doesn't duplicate the data in memory!
    let worker = Worker::new(rx_worker, shared_buffer.clone());
    tokio::spawn(worker.run());

    // Sending a request via the Mediator for the Worker to process ID 1001
    let (tx_respond, rx_respond) = oneshot::channel();
    let msg = Envelope { data_id: 1001, tx_respond };

    tx_worker.send(msg).await.unwrap();

    if let Ok(res) = rx_respond.await {
        println!("[Result]: {}", res);
    }
}

```

### Ready-Made Third-Party Libraries (Advanced Alternatives)

If you don't want to build this channel infrastructure by hand, there are established crates in the Rust ecosystem for this:

1. **For the pure Actor Model:**
* **`actix`**: The most famous actor library in Rust. It natively handles the whole lifecycle management of isolated processes and message exchange under the Mediator pattern.


2. **For very-high-performance Shared Memory Buffers (Zero-Copy):**
* **`bytes`**: Provides the `Bytes` struct, an efficiently reference-counted memory buffer, perfect for network traffic or disk I/O without extra allocations.
* **`shared_memory` / `iceoryx2`**: If these processes were real OS processes (with different PIDs on Linux) instead of threads/tasks within the same program, these crates let you create real IPC (*Inter-Process Communication*) shared memory via the kernel's SHM.


# Second session with Claude

can be seen in first_architectural_draft.md


# My initial idea about the architecture

* The first premise was to use isolated processes rather than threads; the threads should be agnostic and communicate through the mediator
* the lock between the processes wasn't meant to be a mutex but rather blocks of shared memory, where the process would go fetch the information exactly like it happens on the Saturn — the Saturn's own architectural solutions are performant precisely because the processes are independent: they do what they need to do without caring about a global state, since that state is shared through buffers, DMA, and shared memory, and it's the game itself, through its instructions, that controls when things get released, cleared, or updated
* in the first session with Gemini, it's clear that the library to use was one geared toward processes, not threads.
* it's evident from the Saturn's architecture that it's the game that controls things, not the hardware; my idea is that each process runs as a piece of the hardware and waits for orchestration from the game, not from our implementation — the implementation is "dumb," so to speak: it receives the instruction from the game and executes it. It does this using the buffers and the central mediator that distributes the events
