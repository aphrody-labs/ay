//! The I/O half of an event loop, on one thread: sockets registered with mio, timers, and what keeps the loop alive.
//! A JavaScript event loop turns it between its own tasks; a Rust caller blocks on a future with it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

pub use mio::{Interest, Token};

/// What a registered source is ready for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Readiness {
    pub readable: bool,
    pub writable: bool,
    pub error: bool,
    pub read_closed: bool,
    pub write_closed: bool,
}

/// A registered source's owner. mio is edge-triggered: `ready` reads and writes until they would block.
pub trait Io {
    fn ready(self: Rc<Self>, reactor: &Rc<Reactor>, readiness: Readiness);
}

/// A timer of the reactor, to cancel it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimerId(Instant, u64);

const WAKER: Token = Token(usize::MAX);

struct Flag {
    woken: AtomicBool,
    waker: mio::Waker,
}

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
        let _ = self.waker.wake();
    }
}

type TimerFn = Box<dyn FnOnce(&Rc<Reactor>)>;

pub struct Reactor {
    poll: RefCell<mio::Poll>,
    events: RefCell<mio::Events>,
    sources: RefCell<Vec<Option<Rc<dyn Io>>>>,
    free: RefCell<Vec<usize>>,
    timers: RefCell<BTreeMap<TimerId, TimerFn>>,
    next_timer: Cell<u64>,
    refs: Cell<usize>,
    flag: Arc<Flag>,
}

impl Reactor {
    pub fn new() -> io::Result<Rc<Reactor>> {
        let poll = mio::Poll::new()?;
        let waker = mio::Waker::new(poll.registry(), WAKER)?;
        Ok(Rc::new(Reactor {
            poll: RefCell::new(poll),
            events: RefCell::new(mio::Events::with_capacity(256)),
            sources: RefCell::default(),
            free: RefCell::default(),
            timers: RefCell::default(),
            next_timer: Cell::new(0),
            refs: Cell::new(0),
            flag: Arc::new(Flag {
                woken: AtomicBool::new(false),
                waker,
            }),
        }))
    }

    /// mio's epoll set: readable while a turn would find something ready, so another loop can wait for it.
    #[cfg(unix)]
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.poll.borrow().as_raw_fd()
    }

    /// A waker that makes the next [`turn`](Self::turn) return, from any thread.
    pub fn waker(&self) -> Waker {
        Waker::from(Arc::clone(&self.flag))
    }

    /// Whether a waker was woken since the last call.
    pub fn take_woken(&self) -> bool {
        self.flag.woken.swap(false, Ordering::AcqRel)
    }

    pub fn register(
        &self,
        source: &mut impl mio::event::Source,
        interest: Interest,
        io: Rc<dyn Io>,
    ) -> io::Result<Token> {
        let index = match self.free.borrow_mut().pop() {
            Some(index) => {
                self.sources.borrow_mut()[index] = Some(io);
                index
            }
            None => {
                let mut sources = self.sources.borrow_mut();
                sources.push(Some(io));
                sources.len() - 1
            }
        };
        let token = Token(index);
        if let Err(error) = self.poll.borrow().registry().register(source, token, interest) {
            self.release(token);
            return Err(error);
        }
        Ok(token)
    }

    pub fn reregister(
        &self,
        source: &mut impl mio::event::Source,
        token: Token,
        interest: Interest,
    ) -> io::Result<()> {
        self.poll.borrow().registry().reregister(source, token, interest)
    }

    pub fn deregister(&self, source: &mut impl mio::event::Source, token: Token) {
        let _ = self.poll.borrow().registry().deregister(source);
        self.release(token);
    }

    fn release(&self, token: Token) {
        if let Some(slot) = self.sources.borrow_mut().get_mut(token.0)
            && slot.take().is_some()
        {
            self.free.borrow_mut().push(token.0);
        }
    }

    /// Runs `f` at `at`, or in the first turn after it.
    pub fn set_timer(&self, at: Instant, f: impl FnOnce(&Rc<Reactor>) + 'static) -> TimerId {
        let id = TimerId(at, self.next_timer.get());
        self.next_timer.set(id.1 + 1);
        self.timers.borrow_mut().insert(id, Box::new(f));
        id
    }

    pub fn cancel_timer(&self, id: TimerId) {
        self.timers.borrow_mut().remove(&id);
    }

    /// When the earliest timer is due.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.borrow().keys().next().map(|id| id.0)
    }

    /// Something that keeps the loop alive began: a listening server, a request in flight.
    pub fn ref_(&self) {
        self.refs.set(self.refs.get() + 1);
    }

    pub fn unref(&self) {
        self.refs.set(self.refs.get().saturating_sub(1));
    }

    /// Whether something keeps the loop alive.
    pub fn is_alive(&self) -> bool {
        self.refs.get() > 0
    }

    /// Waits for readiness until `timeout` (`None`: until something happens), shortened to the next timer, then
    /// runs the owners of the ready sources and the due timers.
    pub fn turn(self: &Rc<Self>, timeout: Option<Duration>) -> io::Result<()> {
        let now = Instant::now();
        let timeout = match (timeout, self.next_deadline()) {
            (Some(timeout), Some(at)) => Some(timeout.min(at.saturating_duration_since(now))),
            (None, Some(at)) => Some(at.saturating_duration_since(now)),
            (timeout, None) => timeout,
        };
        let ready: Vec<(Token, Readiness)> = {
            let mut events = self.events.borrow_mut();
            match self.poll.borrow_mut().poll(&mut events, timeout) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            events
                .iter()
                .map(|event| {
                    (
                        event.token(),
                        Readiness {
                            readable: event.is_readable(),
                            writable: event.is_writable(),
                            error: event.is_error(),
                            read_closed: event.is_read_closed(),
                            write_closed: event.is_write_closed(),
                        },
                    )
                })
                .collect()
        };
        for (token, readiness) in ready {
            if token == WAKER {
                continue;
            }
            let io = self.sources.borrow().get(token.0).cloned().flatten();
            if let Some(io) = io {
                io.ready(self, readiness);
            }
        }
        self.fire_timers();
        Ok(())
    }

    /// Runs the timers that are due, in the order they are due.
    pub fn fire_timers(self: &Rc<Self>) {
        let now = Instant::now();
        loop {
            let due = {
                let mut timers = self.timers.borrow_mut();
                match timers.first_key_value() {
                    Some((id, _)) if id.0 <= now => timers.pop_first(),
                    _ => None,
                }
            };
            let Some((_, f)) = due else { break };
            f(self);
        }
    }

    /// Runs the reactor until `future` is done.
    pub fn block_on<F: Future>(self: &Rc<Self>, future: F) -> F::Output {
        let mut future = pin!(future);
        let waker = self.waker();
        let mut cx = Context::from_waker(&waker);
        loop {
            self.take_woken();
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
            while !self.take_woken() {
                if self.turn(None).is_err() {
                    break;
                }
            }
        }
    }
}
