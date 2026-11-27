//! Bun's event loop on Boa: Boa's job executor, with Bun's turn (src/jsc/event_loop.rs, `auto_tick` in
//! src/runtime/jsc_hooks.rs): tasks and microtasks, then the immediates queued before the turn, then a tick of the
//! thread's uSockets loop (no longer than the next timer, not at all if work is queued), then the timers that are
//! due. `bun_http_rs`'s reactor is one poll of that loop, turned after each tick. Microtasks run after
//! each callback. The process stays alive while a timer, an immediate or an I/O handle (a listening server, a request
//! in flight) is ref'd.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::{Duration, Instant};

use boa_engine::job::{GenericJob, IntervalJob, Job, JobExecutor, NativeAsyncJob, PromiseJob, TimeoutJob};
use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsValue};
use bun_http_rs::Reactor;

/// A callback for the next turn: what I/O completes with.
pub type Task = Box<dyn FnOnce(&mut Context) -> JsResult<()>>;

/// A task that another thread posts to this loop: it carries only `Send` data, and runs on the loop's thread.
pub type RemoteTask = Box<dyn FnOnce(&mut Context) -> JsResult<()> + Send>;

struct RemoteQueue {
    tasks: Mutex<Vec<RemoteTask>>,
    pending: AtomicBool,
    /// The loop stopped: what is posted from now on is dropped, and nothing wakes it.
    closed: AtomicBool,
    /// The thread's uSockets loop, which the event loop waits in: `us_wakeup_loop` makes its tick return.
    us_loop: usize,
}

/// The loop as other threads reach it (`ScriptExecutionContext::postTaskTo` and the loop's wakeup): what they post
/// runs in its next tick, and posting wakes it from its wait.
#[derive(Clone)]
pub struct Remote(Arc<RemoteQueue>);

impl Remote {
    /// Queues `task` and wakes the loop; `false` (and the task is dropped) once the loop has stopped.
    pub fn post(&self, task: RemoteTask) -> bool {
        let mut tasks = self.0.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.closed.load(Ordering::Acquire) {
            return false;
        }
        tasks.push(task);
        self.0.pending.store(true, Ordering::Release);
        self.wake_locked();
        true
    }

    /// Wakes the loop from its wait, so that it looks again at what stops it.
    pub fn wake(&self) {
        let _tasks = self.0.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if !self.0.closed.load(Ordering::Acquire) {
            self.wake_locked();
        }
    }

    /// Under the lock, with the loop not closed: its thread has not left it.
    fn wake_locked(&self) {
        // SAFETY: the thread's loop, alive until the event loop closes its remote, which takes the same lock.
        unsafe { bun_uws_sys::loop_::us_wakeup_loop(self.0.us_loop as *const bun_uws_sys::Loop) };
    }

    /// Whether `other` posts to the same loop.
    pub fn same_loop(&self, other: &Remote) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn take(&self) -> Vec<RemoteTask> {
        if !self.0.pending.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }
        std::mem::take(&mut *self.0.tasks.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn close(&self) -> Vec<RemoteTask> {
        let mut tasks = self.0.tasks.lock().unwrap_or_else(|e| e.into_inner());
        self.0.closed.store(true, Ordering::Release);
        self.0.pending.store(false, Ordering::Release);
        std::mem::take(&mut *tasks)
    }
}

/// What a terminated loop's run returns: script did not throw it, and nothing catches it.
pub fn is_termination(error: &JsError) -> bool {
    matches!(
        error.as_engine(),
        Some(boa_engine::error::EngineError::RuntimeLimit(boa_engine::error::RuntimeLimitError::Terminated))
    )
}

fn terminated() -> JsError {
    boa_engine::error::RuntimeLimitError::Terminated.into()
}

enum Queued {
    Generic(GenericJob),
    Native(Task),
}

pub(crate) enum Callback {
    Function { function: JsObject, args: Vec<JsValue> },
    Timeout(TimeoutJob),
    Interval(IntervalJob),
}

pub(crate) struct Timer {
    pub callback: Callback,
    /// For an interval.
    pub repeat: Option<Duration>,
    pub delay: Duration,
    pub deadline: Instant,
    seq: u64,
    pub refed: bool,
}

pub(crate) struct Immediate {
    pub function: JsObject,
    pub args: Vec<JsValue>,
    pub refed: bool,
}

#[derive(Default)]
struct Timers {
    next_id: u32,
    next_seq: u64,
    active: HashMap<u32, Timer>,
    /// By deadline, then by when they were set.
    queue: BTreeMap<(Instant, u64), u32>,
    refed: usize,
    immediates: HashMap<u32, Immediate>,
    /// The immediates of the next turn, in order.
    immediate_queue: Vec<u32>,
    refed_immediates: usize,
}

/// The loop's one wait: the thread's uSockets loop (`bun_uws_sys::Loop::get()`, which Bun's sockets and its HTTP
/// client tick), with `bun_http_rs`'s mio epoll set in it as one poll while fetch and `Bun.serve` are on that reactor.
struct Wait {
    loop_: &'static bun_uws_sys::ThreadLoop,
    reactor: *mut bun_usockets_rs::us_internal_async,
}

/// The reactor's turn after the tick does what its readiness asks.
unsafe extern "C" fn reactor_ready(_: *mut bun_usockets_rs::us_loop_t) {}

impl Wait {
    fn new(reactor: &Reactor) -> std::io::Result<Wait> {
        let loop_ = bun_uws_sys::Loop::get();
        let raw = std::ptr::from_ref::<bun_uws_sys::Loop>(loop_).cast_mut().cast();
        // SAFETY: this thread's loop, and the reactor's epoll set, which lives as long as the event loop.
        let poll = unsafe { bun_usockets_rs::us_internal_create_callback_poll(raw, reactor.as_raw_fd(), reactor_ready) };
        if poll.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Wait { loop_, reactor: poll })
    }

    /// Waits until a socket or the reactor is ready, no longer than `timeout` (`None`: as long as it takes), and runs
    /// what uSockets dispatches.
    fn wait(&self, timeout: Option<Duration>) {
        let timeout = timeout.map(|t| bun_core::Timespec {
            sec: t.as_secs() as i64,
            nsec: i64::from(t.subsec_nanos()),
        });
        self.loop_.tick_with_timeout(timeout.as_ref(), bun_uws_sys::NOW_NS_UNKNOWN);
    }

    /// Whether a socket keeps the loop alive (uSockets' `active` count, which the reactor's poll is not in).
    fn is_active(&self) -> bool {
        self.loop_.is_active()
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        // SAFETY: the poll made in `new`, on this thread's loop.
        unsafe { bun_usockets_rs::us_internal_callback_poll_free(self.reactor) };
    }
}

pub struct EventLoop {
    reactor: Rc<Reactor>,
    wait: Wait,
    microtasks: RefCell<VecDeque<PromiseJob>>,
    async_jobs: RefCell<VecDeque<NativeAsyncJob>>,
    tasks: RefCell<VecDeque<Queued>>,
    timers: RefCell<Timers>,
    /// The timer whose callback is running: `refresh()` and `clearInterval` inside it act on it.
    running: Cell<Option<u32>>,
    /// The error a callback threw that nothing caught.
    uncaught: RefCell<Option<JsError>>,
    work: crate::work_pool::WorkQueue,
    remote: Remote,
    /// Set from any thread (`worker.terminate()`): the loop stops at its next step, as does the script it runs.
    terminating: Arc<AtomicBool>,
}

impl EventLoop {
    pub fn new() -> std::io::Result<Rc<EventLoop>> {
        let reactor = Reactor::new()?;
        let wait = Wait::new(&reactor)?;
        let remote = Remote(Arc::new(RemoteQueue {
            tasks: Mutex::default(),
            pending: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            us_loop: std::ptr::from_ref::<bun_uws_sys::Loop>(wait.loop_) as usize,
        }));
        Ok(Rc::new(EventLoop {
            wait,
            reactor,
            remote,
            terminating: Arc::new(AtomicBool::new(false)),
            microtasks: RefCell::default(),
            async_jobs: RefCell::default(),
            tasks: RefCell::default(),
            timers: RefCell::default(),
            running: Cell::new(None),
            uncaught: RefCell::default(),
            work: crate::work_pool::WorkQueue::default(),
        }))
    }

    pub(crate) fn work_queue(&self) -> &crate::work_pool::WorkQueue {
        &self.work
    }

    /// The loop of the realm.
    pub fn of(context: &Context) -> Rc<EventLoop> {
        context
            .downcast_job_executor::<EventLoop>()
            .expect("the realm runs on bun_runtime_js's event loop")
    }

    pub fn reactor(&self) -> &Rc<Reactor> {
        &self.reactor
    }

    /// What other threads post to this loop with.
    pub fn remote(&self) -> Remote {
        self.remote.clone()
    }

    /// The flag that terminates this loop and its script (see [`Context::set_interrupt`]).
    pub fn terminating(&self) -> Arc<AtomicBool> {
        self.terminating.clone()
    }

    pub fn is_terminating(&self) -> bool {
        self.terminating.load(Ordering::Relaxed)
    }

    fn check_terminating(&self) -> JsResult<()> {
        if self.is_terminating() { Err(terminated()) } else { Ok(()) }
    }

    /// The loop is done: other threads can post no more, and what they posted is dropped (here, on this thread).
    pub fn close_remote(&self) {
        drop(self.remote.close());
    }

    fn take_remote(&self) {
        let remote = self.remote.take();
        if remote.is_empty() {
            return;
        }
        let mut tasks = self.tasks.borrow_mut();
        for task in remote {
            tasks.push_back(Queued::Native(task));
        }
    }

    /// Queues `task` for the next tick, as a task posted to Bun's loop.
    pub fn enqueue(&self, task: impl FnOnce(&mut Context) -> JsResult<()> + 'static) {
        self.tasks.borrow_mut().push_back(Queued::Native(Box::new(task)));
    }

    fn has_queued_work(&self) -> bool {
        !self.microtasks.borrow().is_empty()
            || !self.async_jobs.borrow().is_empty()
            || !self.tasks.borrow().is_empty()
            || self.remote.0.pending.load(Ordering::Acquire)
    }

    /// Whether anything could still run JavaScript: queued work, a ref'd timer or immediate, a ref'd I/O handle.
    pub fn is_alive(&self) -> bool {
        let timers = self.timers.borrow();
        self.has_queued_work()
            || timers.refed > 0
            || timers.refed_immediates > 0
            || self.reactor.is_alive()
            || self.wait.is_active()
    }

    fn report(&self, error: JsError) {
        if is_termination(&error) {
            self.terminating.store(true, Ordering::Relaxed);
            return;
        }
        let mut uncaught = self.uncaught.borrow_mut();
        if uncaught.is_none() {
            *uncaught = Some(error);
        }
    }

    fn take_uncaught(&self) -> JsResult<()> {
        match self.uncaught.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Runs the microtasks (promise reactions, then module loading) until none are queued.
    pub fn drain_microtasks(&self, context: &mut Context) -> JsResult<()> {
        loop {
            self.check_terminating()?;
            let job = self.microtasks.borrow_mut().pop_front();
            if let Some(job) = job {
                if let Err(error) = job.call(context) {
                    self.report(error);
                }
                continue;
            }
            let job = self.async_jobs.borrow_mut().pop_front();
            let Some(job) = job else { break };
            let cell = RefCell::new(&mut *context);
            let mut future = job.call(&cell);
            // Boa's own async jobs (module loading, dynamic import) are done after one poll, the module loader
            // being synchronous; one that is not is dropped.
            let mut cx = TaskContext::from_waker(Waker::noop());
            if let Poll::Ready(Err(error)) = std::pin::Pin::new(&mut future).poll(&mut cx) {
                drop(future);
                self.report(error);
            }
        }
        context.clear_kept_objects();
        self.check_terminating()?;
        self.take_uncaught()
    }

    /// Bun's `tick`: the queued tasks (with what other threads posted), each followed by the microtasks.
    pub fn tick(&self, context: &mut Context) -> JsResult<()> {
        self.drain_microtasks(context)?;
        loop {
            self.take_remote();
            let task = self.tasks.borrow_mut().pop_front();
            let Some(task) = task else { break };
            let result = match task {
                Queued::Generic(job) => job.call(context).map(drop),
                Queued::Native(task) => task(context),
            };
            if let Err(error) = result {
                self.report(error);
            }
            self.drain_microtasks(context)?;
        }
        Ok(())
    }

    /// Bun's `autoTick`: the immediates queued before it, one uSockets tick no longer than the next timer, the
    /// reactor's ready sources, the timers that are due.
    fn auto_tick(&self, context: &mut Context) -> JsResult<()> {
        self.run_immediates(context)?;
        let timeout = if self.has_queued_work() || !self.timers.borrow().immediate_queue.is_empty() {
            Some(Duration::ZERO)
        } else {
            let now = Instant::now();
            [self.next_deadline(), self.reactor.next_deadline()]
                .into_iter()
                .flatten()
                .min()
                .map(|at| at.saturating_duration_since(now))
        };
        if !self.reactor.is_alive() && !self.wait.is_active() && timeout.is_none() {
            return Ok(());
        }
        self.wait.wait(timeout);
        self.reactor
            .turn(Some(Duration::ZERO))
            .map_err(|error| JsNativeError::error().with_message(error.to_string()))?;
        self.reactor.take_woken();
        self.take_completed_work();
        self.tick(context)?;
        self.run_timers(context)
    }

    /// Runs the loop until `done` or until nothing is left that could run JavaScript.
    pub fn run_until(&self, context: &mut Context, mut done: impl FnMut() -> bool) -> JsResult<()> {
        loop {
            self.check_terminating()?;
            self.tick(context)?;
            if done() || !self.is_alive() {
                return Ok(());
            }
            self.auto_tick(context)?;
        }
    }

    /// Runs the loop until nothing is left that could run JavaScript.
    pub fn run(&self, context: &mut Context) -> JsResult<()> {
        self.run_until(context, || false)
    }

    // ───────────────────────────── timers ─────────────────────────────

    fn next_deadline(&self) -> Option<Instant> {
        let timers = self.timers.borrow();
        timers.queue.keys().next().map(|(at, _)| *at)
    }

    pub(crate) fn add_timer(&self, callback: Callback, delay: Duration, repeat: bool) -> u32 {
        let mut timers = self.timers.borrow_mut();
        timers.next_id = timers.next_id.wrapping_add(1).max(1);
        let id = timers.next_id;
        let seq = timers.next_seq;
        timers.next_seq += 1;
        let deadline = Instant::now() + delay;
        timers.queue.insert((deadline, seq), id);
        timers.refed += 1;
        timers.active.insert(
            id,
            Timer {
                callback,
                repeat: repeat.then_some(delay),
                delay,
                deadline,
                seq,
                refed: true,
            },
        );
        id
    }

    pub(crate) fn clear_timer(&self, id: u32) {
        let mut timers = self.timers.borrow_mut();
        if let Some(timer) = timers.active.remove(&id) {
            timers.queue.remove(&(timer.deadline, timer.seq));
            if timer.refed {
                timers.refed -= 1;
            }
        }
        // A Boa interval clearing itself from its callback.
        if self.running.get() == Some(id) {
            self.running.set(None);
        }
    }

    pub(crate) fn has_timer(&self, id: u32) -> bool {
        self.timers.borrow().active.contains_key(&id)
    }

    pub(crate) fn set_timer_ref(&self, id: u32, refed: bool) {
        let mut timers = self.timers.borrow_mut();
        let Some(timer) = timers.active.get_mut(&id) else { return };
        if timer.refed == refed {
            return;
        }
        timer.refed = refed;
        if refed {
            timers.refed += 1;
        } else {
            timers.refed -= 1;
        }
    }

    pub(crate) fn timer_has_ref(&self, id: u32) -> bool {
        self.timers.borrow().active.get(&id).is_some_and(|t| t.refed)
    }

    /// `timeout.refresh()`: due its delay from now, again.
    pub(crate) fn refresh_timer(&self, id: u32) {
        let mut timers = self.timers.borrow_mut();
        let seq = timers.next_seq;
        timers.next_seq += 1;
        let Some(timer) = timers.active.get_mut(&id) else { return };
        let old = (timer.deadline, timer.seq);
        timer.deadline = Instant::now() + timer.delay;
        timer.seq = seq;
        let key = (timer.deadline, seq);
        timers.queue.remove(&old);
        timers.queue.insert(key, id);
    }

    fn run_timers(&self, context: &mut Context) -> JsResult<()> {
        let now = Instant::now();
        loop {
            let due = {
                let mut timers = self.timers.borrow_mut();
                match timers.queue.first_key_value() {
                    Some((&(at, _), _)) if at <= now => timers.queue.pop_first().map(|(_, id)| id),
                    _ => None,
                }
            };
            let Some(id) = due else { break };
            let result = self.fire(id, context);
            // The timer stays active while its callback runs, so that the callback can unref, refresh or clear it.
            let mut timers = self.timers.borrow_mut();
            let next_seq = timers.next_seq;
            if let Some(timer) = timers.active.get_mut(&id) {
                let refreshed_by_callback = timer.deadline > now;
                match timer.repeat {
                    _ if refreshed_by_callback => {}
                    Some(interval) => {
                        timer.deadline = Instant::now() + interval;
                        timer.seq = next_seq;
                        let key = (timer.deadline, timer.seq);
                        timers.next_seq += 1;
                        timers.queue.insert(key, id);
                    }
                    None => {
                        let refed = timer.refed;
                        timers.active.remove(&id);
                        if refed {
                            timers.refed -= 1;
                        }
                    }
                }
            }
            drop(timers);
            if let Err(error) = result {
                self.report(error);
            }
            self.drain_microtasks(context)?;
        }
        Ok(())
    }

    /// Calls the callback of timer `id`, which is due.
    fn fire(&self, id: u32, context: &mut Context) -> JsResult<()> {
        let mut timers = self.timers.borrow_mut();
        let Some(timer) = timers.active.get(&id) else { return Ok(()) };
        if let Callback::Function { function, args } = &timer.callback {
            let (function, args) = (function.clone(), args.clone());
            drop(timers);
            return function.call(&JsValue::undefined(), &args, context).map(drop);
        }
        // Boa's own jobs: out of the table while they run.
        let Some(timer) = timers.active.remove(&id) else { return Ok(()) };
        if timer.refed {
            timers.refed -= 1;
        }
        drop(timers);
        self.running.set(Some(id));
        let result = match timer.callback {
            Callback::Timeout(job) => return job.call(context).map(drop),
            Callback::Interval(ref job) => job.call(context).map(drop),
            Callback::Function { .. } => Ok(()),
        };
        if self.running.replace(None) == Some(id) {
            let mut timers = self.timers.borrow_mut();
            if timer.refed {
                timers.refed += 1;
            }
            timers.active.insert(id, timer);
        }
        result
    }

    // ───────────────────────────── immediates ─────────────────────────────

    pub(crate) fn add_immediate(&self, function: JsObject, args: Vec<JsValue>) -> u32 {
        let mut timers = self.timers.borrow_mut();
        timers.next_id = timers.next_id.wrapping_add(1).max(1);
        let id = timers.next_id;
        timers.immediates.insert(
            id,
            Immediate {
                function,
                args,
                refed: true,
            },
        );
        timers.immediate_queue.push(id);
        timers.refed_immediates += 1;
        id
    }

    pub(crate) fn clear_immediate(&self, id: u32) {
        let mut timers = self.timers.borrow_mut();
        if let Some(immediate) = timers.immediates.remove(&id)
            && immediate.refed
        {
            timers.refed_immediates -= 1;
        }
    }

    pub(crate) fn set_immediate_ref(&self, id: u32, refed: bool) {
        let mut timers = self.timers.borrow_mut();
        let Some(immediate) = timers.immediates.get_mut(&id) else { return };
        if immediate.refed == refed {
            return;
        }
        immediate.refed = refed;
        if refed {
            timers.refed_immediates += 1;
        } else {
            timers.refed_immediates -= 1;
        }
    }

    pub(crate) fn immediate_has_ref(&self, id: u32) -> bool {
        self.timers.borrow().immediates.get(&id).is_some_and(|i| i.refed)
    }

    /// The immediates queued before this call; those they queue run in the next turn.
    fn run_immediates(&self, context: &mut Context) -> JsResult<()> {
        let batch = std::mem::take(&mut self.timers.borrow_mut().immediate_queue);
        for id in batch {
            let immediate = {
                let mut timers = self.timers.borrow_mut();
                let immediate = timers.immediates.remove(&id);
                if immediate.as_ref().is_some_and(|i| i.refed) {
                    timers.refed_immediates -= 1;
                }
                immediate
            };
            let Some(immediate) = immediate else { continue };
            if let Err(error) = immediate.function.call(&JsValue::undefined(), &immediate.args, context) {
                self.report(error);
            }
            self.drain_microtasks(context)?;
        }
        Ok(())
    }
}

impl JobExecutor for EventLoop {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        match bun_js::carry_async_context(job, context) {
            Job::PromiseJob(job) => self.microtasks.borrow_mut().push_back(job),
            Job::AsyncJob(job) => self.async_jobs.borrow_mut().push_back(job),
            Job::GenericJob(job) => self.tasks.borrow_mut().push_back(Queued::Generic(job)),
            Job::TimeoutJob(job) => {
                let delay = Duration::from_millis(job.timeout().as_millis());
                self.add_timer(Callback::Timeout(job), delay, false);
            }
            Job::IntervalJob(job) => {
                let delay = Duration::from_millis(job.interval().as_millis());
                self.add_timer(Callback::Interval(job), delay, true);
            }
            // A cleanup waits for the collector, which this loop does not run.
            Job::FinalizationRegistryCleanupJob(_) => {}
            _ => {}
        }
    }

    /// Runs what is queued without waiting: microtasks, then tasks. The loop itself runs from [`EventLoop::run`].
    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        self.drain_microtasks(context)
    }
}
