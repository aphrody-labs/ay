//! Work on the work pool, whose result comes back to the event loop: `EventLoop::run_on_work_pool`.

use bun_js::{JSGlobalObject, JSPromise, JSValue};
use bun_runtime_js::EventLoop;
use bun_sema_standalone as _;

/// A realm on the event loop, in a process whose stdio is set up as Bun's `main` sets it up.
fn realm(f: impl FnOnce(&JSGlobalObject, &EventLoop)) {
    bun_core::output::stdio::init();
    let mut context = boa_engine::Context::builder()
        .job_executor(EventLoop::new().expect("an event loop"))
        .build()
        .expect("a realm");
    let event_loop = EventLoop::of(&context);
    f(JSGlobalObject::from_context(&mut context), &event_loop)
}

fn run(global: &JSGlobalObject, event_loop: &EventLoop) -> Result<(), String> {
    // SAFETY: nothing else reaches the realm while its loop runs, but what the loop calls.
    let context = unsafe { global.context() };
    event_loop.run(context).map_err(|error| {
        let error = global.throw_engine_error(error);
        global.take_exception(error).to_utf8(global).unwrap()
    })
}

fn eval(global: &JSGlobalObject, source: &str) -> JSValue {
    match global.evaluate_script(source) {
        Ok(value) => value,
        Err(error) => panic!("{source}: threw {:?}", global.take_exception(error).to_utf8(global)),
    }
}

fn text(global: &JSGlobalObject, source: &str) -> String {
    eval(global, source).to_utf8(global).unwrap()
}

#[test]
fn a_promise_settles_with_what_the_work_returns() {
    realm(|global, event_loop| {
        let promise = JSPromise::create(global);
        global.to_js_value().put(global, "p", promise.promise());
        eval(global, "var got = []; p.then(v => got.push(v));");
        event_loop.run_on_work_pool(
            || 6 * 7,
            move |context, n| {
                let global = JSGlobalObject::from_context(context);
                let resolved = promise.resolve(global, JSValue::js_number(f64::from(n)));
                resolved.map_err(|error| global.into_engine_error(error))
            },
        );
        // Nothing comes back before the loop runs, and the loop waits for it.
        assert_eq!(text(global, "got.length"), "0");
        run(global, event_loop).unwrap();
        assert_eq!(text(global, "JSON.stringify(got)"), "[42]");
    });
}

#[test]
fn the_work_runs_off_the_javascript_thread() {
    realm(|global, event_loop| {
        let here = std::thread::current().id();
        let done = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = done.clone();
        event_loop.run_on_work_pool(
            move || std::thread::current().id() != here,
            move |_, elsewhere| {
                seen.set(Some(elsewhere));
                Ok(())
            },
        );
        run(global, event_loop).unwrap();
        assert_eq!(done.get(), Some(true));
    });
}

#[test]
fn completions_come_back_in_any_order_and_each_once() {
    realm(|global, event_loop| {
        eval(global, "var got = [];");
        for i in 0..16u32 {
            let promise = JSPromise::create(global);
            global.to_js_value().put(global, "p", promise.promise());
            eval(global, "p.then(v => got.push(v));");
            event_loop.run_on_work_pool(
                move || {
                    std::thread::sleep(std::time::Duration::from_millis(u64::from(16 - i)));
                    i
                },
                move |context, n| {
                    let global = JSGlobalObject::from_context(context);
                    let resolved = promise.resolve(global, JSValue::js_number(f64::from(n)));
                    resolved.map_err(|error| global.into_engine_error(error))
                },
            );
        }
        run(global, event_loop).unwrap();
        assert_eq!(
            text(global, "JSON.stringify(got.sort((a, b) => a - b))"),
            "[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]"
        );
    });
}

#[test]
fn an_exception_of_done_is_uncaught() {
    realm(|global, event_loop| {
        event_loop.run_on_work_pool(
            || (),
            |context, ()| {
                let global = JSGlobalObject::from_context(context);
                let error = global.throw(format_args!("from done"));
                Err(global.into_engine_error(error))
            },
        );
        assert_eq!(run(global, event_loop).unwrap_err(), "Error: from done");
    });
}

#[test]
fn a_panic_of_the_work_unwinds_on_the_javascript_thread() {
    realm(|global, event_loop| {
        event_loop.run_on_work_pool(|| -> u32 { panic!("in the pool") }, |_, _| Ok(()));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(global, event_loop)));
        let payload = unwound.expect_err("the panic comes back");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"in the pool"));
    });
}
