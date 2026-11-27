//! Work off the JavaScript thread: a job runs on Bun's work pool (`bun_threading::WorkPool`), and what it returns comes
//! back to the realm's [`EventLoop`] as a task of its next turn, as a `ConcurrentTask` comes back to Bun's loop
//! (src/jsc/event_loop.rs). Until then the loop stays alive, as a ref'd `KeepAlive` keeps it.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::task::Waker;

use boa_engine::{Context, JsResult};
use bun_threading::{Guarded, WorkPool};

use crate::EventLoop;

type Value = Box<dyn Any + Send>;
type Done = Box<dyn FnOnce(&mut Context, Value) -> JsResult<()>>;
/// What a job returned, or the panic it unwound with, which unwinds again on the JavaScript thread.
type Completed = (u64, std::thread::Result<Value>);

/// The jobs of a loop: what is waiting for its work, and the work that is done.
#[derive(Default)]
pub(crate) struct WorkQueue {
    next: Cell<u64>,
    waiting: RefCell<HashMap<u64, Done>>,
    completed: Arc<Guarded<Vec<Completed>>>,
}

struct Job {
    id: u64,
    work: Box<dyn FnOnce() -> Value + Send>,
    completed: Arc<Guarded<Vec<Completed>>>,
    /// The reactor's: its next turn returns.
    waker: Waker,
}

fn run(job: Job) {
    let result = catch_unwind(AssertUnwindSafe(job.work));
    job.completed.lock().push((job.id, result));
    job.waker.wake();
}

impl EventLoop {
    /// Runs `work` on the work pool, then `done` with what it returned, as a task of this loop. The process has set up
    /// Bun's stdio (`bun_core::output::stdio::init`), as Bun's `main` does: a thread of the pool configures its output
    /// from it.
    pub fn run_on_work_pool<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Context, T) -> JsResult<()> + 'static,
    ) {
        let queue = self.work_queue();
        let id = queue.next.get();
        queue.next.set(id + 1);
        let done: Done = Box::new(move |context, value| {
            let value = value.downcast::<T>().expect("a job's value is of the type its work returns");
            done(context, *value)
        });
        queue.waiting.borrow_mut().insert(id, done);
        self.reactor().ref_();
        let job = Job {
            id,
            work: Box::new(move || Box::new(work()) as Value),
            completed: Arc::clone(&queue.completed),
            waker: self.reactor().waker(),
        };
        bun_core::handle_oom(WorkPool::go(job, run));
    }

    /// Makes the work that is done tasks of the next tick: what the loop does after each wait.
    pub(crate) fn take_completed_work(&self) {
        let queue = self.work_queue();
        let completed = std::mem::take(&mut *queue.completed.lock());
        for (id, result) in completed {
            let Some(done) = queue.waiting.borrow_mut().remove(&id) else { continue };
            self.reactor().unref();
            self.enqueue(move |context| {
                let value = result.unwrap_or_else(|panic| resume_unwind(panic));
                done(context, value)
            });
        }
    }
}
