//! `ByteLengthQueuingStrategy` and `CountQueuingStrategy` (Streams §7): JSByteLengthQueuingStrategy.cpp and
//! JSCountQueuingStrategy.cpp. Each realm's `size` function is one function, which the getter answers every time.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, JSFunction, JSGlobalObject, JSValue, JsClass, JsResult};

/// `QueuingStrategyInit`: an object with a `highWaterMark`.
fn high_water_mark_of(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<f64> {
    if frame.arguments_count() < 1 {
        return Err(global.throw_type_error(format_args!("Not enough arguments")));
    }
    let init = frame.argument(0);
    if !init.is_object() {
        return Err(global.throw_type_error(format_args!("The QueuingStrategyInit argument must be an object")));
    }
    let Some(high_water_mark) = init.get(global, "highWaterMark")? else {
        return Err(global.throw_type_error(format_args!("QueuingStrategyInit requires a 'highWaterMark' member")));
    };
    high_water_mark.to_number(global)
}

/// The realm's `size` function of `T`, made the first time.
fn size_function(global: &JSGlobalObject, key: &str, make: fn(&JSGlobalObject) -> JSValue) -> JsResult<JSValue> {
    let holder = global.to_js_value();
    let cached = holder.get(global, key)?;
    if let Some(cached) = cached {
        return Ok(cached);
    }
    let function = make(global);
    crate::define_global(crate::cx(global), key, function.clone().into_engine())
        .map_err(|error| global.throw_engine_error(error))?;
    Ok(function)
}

#[derive(Trace, Finalize)]
pub struct CountQueuingStrategy {
    high_water_mark: f64,
}

impl JsClass for CountQueuingStrategy {
    const NAME: &'static str = "CountQueuingStrategy";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        Ok(CountQueuingStrategy {
            high_water_mark: high_water_mark_of(global, frame)?,
        })
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("highWaterMark", attributes, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.high_water_mark))
        })?;
        class.getter("size", attributes, |_, global| {
            size_function(global, "__intrinsic__countQueuingStrategySize", |global| {
                JSFunction::create(global, "size", |_, _| Ok(JSValue::js_number(1.0)), 0)
            })
        })?;
        Ok(())
    }
}

#[derive(Trace, Finalize)]
pub struct ByteLengthQueuingStrategy {
    high_water_mark: f64,
}

impl JsClass for ByteLengthQueuingStrategy {
    const NAME: &'static str = "ByteLengthQueuingStrategy";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        Ok(ByteLengthQueuingStrategy {
            high_water_mark: high_water_mark_of(global, frame)?,
        })
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("highWaterMark", attributes, |this, global| {
            Ok(JSValue::js_number(this.borrow(global)?.high_water_mark))
        })?;
        class.getter("size", attributes, |_, global| {
            size_function(global, "__intrinsic__byteLengthQueuingStrategySize", |global| {
                JSFunction::create(
                    global,
                    "size",
                    |global, frame| Ok(frame.argument(0).get(global, "byteLength")?.unwrap_or_default()),
                    1,
                )
            })
        })?;
        Ok(())
    }
}
