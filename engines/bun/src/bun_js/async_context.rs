//! The async context of a realm: JavaScriptCore's `$asyncContext`, the frame of the AsyncLocalStorage stores that
//! src/js/node/async_hooks.ts keeps in its internal field 0. Once tracking is on (NodeAsyncHooks.cpp's
//! `jsSetAsyncHooksEnabled`), each promise job puts back, while it runs, the context that was in place when it was
//! queued, as JavaScriptCore's microtasks do: `await`, `then` and `queueMicrotask` carry it.

use std::rc::Rc;

use boa_engine::job::{Job, JobExecutor, PromiseJob, SimpleJobExecutor};
use boa_engine::{Context, JsResult as EngineResult, JsValue};

use crate::{JSGlobalObject, JSValue};

#[derive(Default)]
struct AsyncContext {
    value: JsValue,
    tracking: bool,
}

fn slot(context: &mut Context) -> &mut AsyncContext {
    let data = context.host_defined_mut();
    if data.get::<AsyncContext>().is_none() {
        data.insert(AsyncContext::default());
    }
    data.get_mut::<AsyncContext>().expect("inserted above")
}

fn replace(context: &mut Context, value: JsValue) -> JsValue {
    core::mem::replace(&mut slot(context).value, value)
}

impl JSGlobalObject {
    /// What `$getInternalField($asyncContext, 0)` reads.
    pub fn async_context(&self) -> JSValue {
        JSValue(slot(self.cx()).value.clone())
    }

    /// What `$putInternalField($asyncContext, 0, value)` writes.
    pub fn set_async_context(&self, value: JSValue) {
        slot(self.cx()).value = value.0;
    }

    /// From now on, a promise job runs in the async context it was queued in.
    pub fn enable_async_context_tracking(&self) {
        slot(self.cx()).tracking = true;
    }

    pub fn is_async_context_tracking_enabled(&self) -> bool {
        slot(self.cx()).tracking
    }
}

/// `job` as a job executor queues it: once tracking is on, a promise job runs in the async context it was queued in.
pub fn carry_async_context(job: Job, context: &mut Context) -> Job {
    match job {
        Job::PromiseJob(job) if slot(context).tracking => {
            let captured = slot(context).value.clone();
            Job::PromiseJob(PromiseJob::new(move |context| {
                let previous = replace(context, captured);
                let result = job.call(context);
                replace(context, previous);
                result
            }))
        }
        job => job,
    }
}

/// Boa's [`SimpleJobExecutor`], with promise jobs that carry the async context they were queued in.
#[derive(Default)]
pub struct AsyncContextJobExecutor {
    inner: Rc<SimpleJobExecutor>,
}

impl JobExecutor for AsyncContextJobExecutor {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        let job = carry_async_context(job, context);
        Rc::clone(&self.inner).enqueue_job(job, context);
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> EngineResult<()> {
        Rc::clone(&self.inner).run_jobs(context)
    }
}
