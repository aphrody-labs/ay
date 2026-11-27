//! `ResourceUsage`, declared in src/runtime/api/bun/subprocess/ResourceUsage.bind.ts: ResourceUsage.rs on `bun_js`.
//! The objects it gives are without a prototype, `cpuTime` in BigInt microseconds; `contextSwitches`, `cpuTime`
//! and `ops` are the same object each time they are read, as the schema caches them.

use boa_engine::{JsBigInt, JsObject, JsString, JsValue};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};
use bun_spawn_sys::{Rusage, RusageFields as _};

#[derive(Trace, Finalize)]
pub struct ResourceUsage {
    #[unsafe_ignore_trace]
    rusage: Rusage,
    context_switches: Option<JSValue>,
    cpu_time: Option<JSValue>,
    ops: Option<JSValue>,
}

impl ResourceUsage {
    pub fn new(rusage: Rusage) -> ResourceUsage {
        ResourceUsage {
            rusage,
            context_switches: None,
            cpu_time: None,
            ops: None,
        }
    }
}

/// A new object without a prototype of these members, in this order.
fn record(global: &JSGlobalObject, members: &[(&str, JsValue)]) -> JSValue {
    let object = JsObject::with_null_proto();
    for (name, value) in members {
        let _ = object.create_data_property(JsString::from(*name), value.clone(), crate::cx(global));
    }
    JSValue::from_engine(object.into())
}

fn cached(
    this: &ClassRef<ResourceUsage>,
    global: &JSGlobalObject,
    slot: fn(&mut ResourceUsage) -> &mut Option<JSValue>,
    make: fn(&JSGlobalObject, &Rusage) -> JSValue,
) -> JsResult<JSValue> {
    let mut usage = this.borrow_mut(global)?;
    let rusage = usage.rusage;
    Ok(slot(&mut usage).get_or_insert_with(|| make(global, &rusage)).clone())
}

/// What bindgen gives a class's getters.
const GETTER: Attributes = Attributes::DEFAULT;

impl JsClass for ResourceUsage {
    const NAME: &'static str = "ResourceUsage";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.getter("contextSwitches", GETTER, |this, global| {
            cached(this, global, |usage| &mut usage.context_switches, |global, rusage| {
                record(global, &[("voluntary", rusage.nvcsw().into()), ("involuntary", rusage.nivcsw().into())])
            })
        })?;
        class.getter("cpuTime", GETTER, |this, global| {
            cached(this, global, |usage| &mut usage.cpu_time, |global, rusage| {
                let microseconds = |sec: i64, usec: i64| (sec * 1_000_000 + usec) as u64;
                let user = microseconds(rusage.utime_sec(), rusage.utime_usec());
                let system = microseconds(rusage.stime_sec(), rusage.stime_usec());
                let big = |n: u64| JsValue::from(JsBigInt::from(n));
                record(global, &[("user", big(user)), ("system", big(system)), ("total", big(user + system))])
            })
        })?;
        class.getter("maxRSS", GETTER, |this, global| Ok(JSValue::js_number(this.borrow(global)?.rusage.maxrss())))?;
        class.getter("messages", GETTER, |this, global| {
            let rusage = this.borrow(global)?.rusage;
            Ok(record(global, &[("sent", rusage.msgsnd().into()), ("received", rusage.msgrcv().into())]))
        })?;
        class.getter("ops", GETTER, |this, global| {
            cached(this, global, |usage| &mut usage.ops, |global, rusage| {
                record(global, &[("in", rusage.inblock().into()), ("out", rusage.oublock().into())])
            })
        })?;
        class.getter("shmSize", GETTER, |this, global| Ok(JSValue::js_number(this.borrow(global)?.rusage.ixrss())))?;
        class.getter("signalCount", GETTER, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.rusage.nsignals()))
        })?;
        class.getter("swapCount", GETTER, |this, global| Ok(JSValue::js_number(this.borrow(global)?.rusage.nswap())))
    }
}
