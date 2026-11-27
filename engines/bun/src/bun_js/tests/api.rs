//! The engine boundary on Boa: values, objects, host functions, exceptions, roots, modules.

use bun_js::{
    CallFrame, ErrorCode, JSArray, JSFunction, JSGlobalObject, JSModule, JSObject, JSPromise,
    JSValue, JsError, JsResult, Strong, TopExceptionScope, Weak,
};

fn eval(global: &JSGlobalObject, source: &str) -> JSValue {
    match global.evaluate_script(source) {
        Ok(value) => value,
        Err(error) => panic!(
            "{source}: threw {:?}",
            global.take_exception(error).to_utf8(global)
        ),
    }
}

/// `value[key]` as a string, `None` where it is absent.
fn string_of(global: &JSGlobalObject, value: &JSValue, key: &str) -> Option<String> {
    value
        .get(global, key)
        .unwrap()
        .and_then(|value| value.as_string())
}

#[test]
fn values() {
    JSGlobalObject::with_new(|global| {
        assert!(JSValue::UNDEFINED.is_undefined() && JSValue::NULL.is_undefined_or_null());
        assert!(JSValue::js_number_from_int32(-3).is_int32());
        assert!(!JSValue::js_number(0.5).is_int32() && !JSValue::js_number(-0.0).is_int32());
        assert_eq!(
            JSValue::from_utf8("é".as_bytes()).as_string().as_deref(),
            Some("é")
        );
        assert_eq!(
            JSValue::from_utf16(&[0x62, 0x75, 0x6e])
                .to_utf8(global)
                .unwrap(),
            "bun"
        );
        assert_eq!(JSValue::js_number(1e21).to_utf8(global).unwrap(), "1e+21");
        assert_eq!(
            JSValue::js_empty_string(global).get_length(global).unwrap(),
            0
        );
        assert_eq!(eval(global, "'12'").to_number(global).unwrap(), 12.0);
        assert_eq!(eval(global, "4294967297").coerce_to_i32(global).unwrap(), 1);
        assert_eq!(eval(global, "-1").as_int32(), -1);
        // `bun_jsc`'s `to_u32`: truncated and clamped, not `ToUint32`.
        assert_eq!(eval(global, "-1").to_u32(), 0);
        assert_eq!(eval(global, "2 ** 40").to_u32(), u32::MAX);
        assert_eq!(eval(global, "'7'").to_u32(), 0);
        assert_eq!(JSValue::UNDEFINED.as_number(), 0.0);
        assert!(JSValue::from_utf8(b"1").as_number().is_nan());
        assert_eq!(JSValue::NULL.as_number(), 0.0);
        assert_eq!(JSValue::js_boolean(true).as_number(), 1.0);
        assert_eq!(eval(global, "'x'").get_number(), None);
        assert_eq!(eval(global, "10n").js_type_string(global), "bigint");
        assert!(eval(global, "[]").is_array() && eval(global, "new Error('x')").is_error());
        assert!(eval(global, "Symbol()").is_symbol() && eval(global, "Symbol()").is_cell());
        assert!(eval(global, "(() => {})").is_callable());
        assert!(eval(global, "''").is_falsey() && eval(global, "'0'").is_truthy());
        let object = eval(global, "({})");
        assert!(object.is_same_value(object.clone(), global).unwrap());
        assert!(!object.is_same_value(eval(global, "({})"), global).unwrap());
        let nan = JSValue::js_number(f64::NAN);
        assert!(
            nan.is_same_value(nan.clone(), global).unwrap()
                && !nan.is_strict_equal(nan.clone(), global).unwrap()
        );
        assert_eq!(
            JSValue::js_number_from_uint64(u64::MAX).get_number(),
            Some(18446744073709551615.0)
        );
    });
}

#[test]
fn objects_and_arrays() {
    JSGlobalObject::with_new(|global| {
        let object = JSObject::create_from(
            global,
            &[
                ("a", JSValue::js_number(1.0)),
                ("b", JSValue::js_boolean(true)),
            ],
        )
        .unwrap();
        object
            .put_non_enumerable(global, "hidden", JSValue::NULL)
            .unwrap();
        object
            .put_read_only(global, "fixed", JSValue::js_number(7.0))
            .unwrap();
        assert_eq!(object.keys(global).unwrap(), ["a", "b"]);
        // The built-in `Object.keys`, whatever script made of `globalThis.Object`.
        eval(
            global,
            "globalThis.SavedObject = Object; Object.keys = () => ['x']; delete globalThis.Object",
        );
        assert_eq!(object.keys(global).unwrap(), ["a", "b"]);
        eval(global, "globalThis.Object = SavedObject");
        assert!(object.has_property(global, "hidden").unwrap());
        assert_eq!(object.get(global, "fixed").unwrap().get_number(), Some(7.0));
        // `set` is script's assignment, which fails on a read-only property; `put` defines, and does not throw.
        assert_eq!(
            object.set(global, "fixed", JSValue::UNDEFINED),
            Err(JsError::Thrown)
        );
        assert!(global.try_take_exception().unwrap().is_error());
        assert!(object.delete_property(global, "a").unwrap());
        assert!(object.get(global, "a").unwrap().is_undefined());

        let value = object.to_js();
        assert!(value.get(global, "b").unwrap().unwrap().as_boolean());
        assert!(value.get(global, "missing").unwrap().is_none());
        assert!(
            value.get(global, b"hidden").unwrap().unwrap().is_null(),
            "get answers None for undefined only"
        );
        assert!(
            value
                .get_optional::<JSValue>(global, "hidden")
                .unwrap()
                .is_none(),
            "get_optional filters null"
        );
        value.put(global, "n", eval(global, "'12'"));
        assert_eq!(value.get_optional::<i32>(global, "n").unwrap(), Some(12));
        value.put(global, "big", eval(global, "2n ** 64n + 5n"));
        assert_eq!(value.get_optional::<i64>(global, "big").unwrap(), Some(5));
        value.put(global, "negative", eval(global, "-(2n ** 63n) - 1n"));
        assert_eq!(
            value.get_optional::<i64>(global, "negative").unwrap(),
            Some(i64::MAX)
        );
        assert!(JSValue::js_number(1.0).get(global, "x").unwrap().is_none());

        // `put` defines an own data property: a setter on the prototype does not run.
        eval(
            global,
            "globalThis.ran = false; globalThis.Proto = { set trap(v) { ran = true } }",
        );
        let with_setter = eval(global, "Object.create(Proto)");
        with_setter.put(global, "trap", JSValue::js_number(1.0));
        assert!(!eval(global, "ran").to_boolean());
        assert_eq!(
            with_setter
                .get(global, "trap")
                .unwrap()
                .unwrap()
                .get_number(),
            Some(1.0)
        );

        let array = JSArray::create(
            global,
            &[JSValue::js_number(1.0), JSValue::from_utf8(b"two")],
        );
        array.push(global, JSValue::NULL).unwrap();
        assert_eq!(array.len(global).unwrap(), 3);
        let value = array.to_js();
        assert_eq!(value.get_length(global).unwrap(), 3);
        assert_eq!(
            value.get_index(global, 1).unwrap().as_string().as_deref(),
            Some("two")
        );
        let from_slice = JSValue::create_array_from_slice(global, &[JSValue::NULL]).unwrap();
        assert_eq!(from_slice.get_length(global).unwrap(), 1);
        assert_eq!(
            JSValue::create_empty_array(global, 0)
                .unwrap()
                .get_length(global)
                .unwrap(),
            0
        );
        assert_eq!(
            JSValue::create_empty_array(global, 4)
                .unwrap()
                .get_length(global)
                .unwrap(),
            4
        );
        assert_eq!(
            JSValue::from_utf8("bün".as_bytes())
                .get_length(global)
                .unwrap(),
            3
        );
        assert_eq!(
            eval(global, "({ length: 2 ** 60 })")
                .get_length(global)
                .unwrap(),
            (1 << 51) - 1
        );
        assert_eq!(
            eval(global, "({ length: -1 })").get_length(global).unwrap(),
            0
        );
        assert!(JSValue::create_empty_object(global, 3).is_object());
    });
}

fn sum(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let mut total = 0.0;
    for argument in frame.arguments() {
        total += argument.to_number(global)?;
    }
    Ok(JSValue::js_number(total))
}

fn fail(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [message] = frame.arguments_as_array::<1>();
    Err(global.throw_range_error(format_args!("failed: {}", message.to_utf8(global)?)))
}

fn this_of(_: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    Ok(frame.this().clone())
}

fn terminate(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    Err(JsError::Terminated)
}

#[test]
fn host_functions_and_exceptions() {
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        this.put(global, "sum", JSFunction::create(global, "sum", sum, 2));
        this.put(global, "fail", JSFunction::create(global, "fail", fail, 1));
        this.put(
            global,
            "thisOf",
            JSFunction::create(global, "thisOf", this_of, 0),
        );
        assert_eq!(eval(global, "sum(1, '2', 3)").get_number(), Some(6.0));
        assert_eq!(
            eval(global, "sum.name + sum.length").as_string().as_deref(),
            Some("sum2")
        );
        assert_eq!(eval(global, "thisOf.call(5)").get_number(), Some(5.0));
        // A conversion that throws in the host function reaches JavaScript.
        assert_eq!(
            eval(global, "try { sum({ valueOf() { throw new Error('from valueOf') } }) } catch (e) { e.message }")
                .as_string()
                .as_deref(),
            Some("from valueOf")
        );
        assert_eq!(
            eval(
                global,
                "try { fail('x') } catch (e) { `${e instanceof RangeError}:${e.message}` }"
            )
            .as_string()
            .as_deref(),
            Some("true:failed: x")
        );
        assert!(!global.has_exception());

        // Uncaught, it is pending on the global object.
        let error = global.evaluate_script("fail('uncaught')").unwrap_err();
        let scope = TopExceptionScope::new(global);
        assert_eq!(scope.return_if_exception(), Err(JsError::Thrown));
        let exception = scope.exception().unwrap();
        assert!(scope.has_exception(), "exception() leaves it pending");
        assert_eq!(
            string_of(global, &exception, "message").as_deref(),
            Some("failed: uncaught")
        );
        assert_eq!(
            string_of(global, &global.take_exception(error), "name").as_deref(),
            Some("RangeError")
        );
        scope.assert_no_exception();

        // Rust calling JavaScript that throws.
        let thrower = eval(global, "(function () { throw 42 })");
        assert_eq!(
            thrower.call(global, JSValue::UNDEFINED, &[]).map(drop),
            Err(JsError::Thrown)
        );
        assert_eq!(
            global.try_take_exception().unwrap().get_number(),
            Some(42.0)
        );
        assert_eq!(
            JSValue::NULL
                .call(global, JSValue::UNDEFINED, &[])
                .map(drop),
            Err(JsError::Thrown)
        );
        global.clear_exception();
        assert!(!global.has_exception());

        let error = global.create_type_error_instance(format_args!("made {}", 1));
        assert!(error.is_error());
        assert_eq!(
            string_of(global, &error, "message").as_deref(),
            Some("made 1")
        );
        let out_of_memory = global.throw_out_of_memory();
        let taken = global.take_exception(out_of_memory);
        assert_eq!(
            string_of(global, &taken, "message").as_deref(),
            Some("Out of memory")
        );

        let coded = ErrorCode::INVALID_ARG_TYPE.fmt(global, format_args!("bad {}", "arg"));
        this.put(global, "coded", coded);
        assert_eq!(
            eval(global, "`${coded}|${coded.name}|${coded.code}|${coded instanceof TypeError}|${Object.keys(coded)}`")
                .as_string()
                .as_deref(),
            Some("TypeError [ERR_INVALID_ARG_TYPE]: bad arg|TypeError|ERR_INVALID_ARG_TYPE|true|")
        );
        assert_eq!(ErrorCode::ERR_OUT_OF_RANGE, ErrorCode::OUT_OF_RANGE);
        assert_eq!(ErrorCode::ABORT_ERR.name(), "AbortError");
        // `["ERR_INVALID_ARG_VALUE", TypeError, undefined, RangeError]`: no name of its own, and a RangeError twin.
        assert_eq!(ErrorCode::INVALID_ARG_VALUE.name(), "TypeError");
        assert_eq!(
            ErrorCode::INVALID_ARG_VALUE_RangeError.kind(),
            bun_js::ErrorKind::RangeError
        );
        assert_eq!(ErrorCode::INVALID_ARG_VALUE_RangeError.name(), "RangeError");
        assert_eq!(
            ErrorCode::INVALID_STATE_TypeError.code(),
            "ERR_INVALID_STATE"
        );
        let twin = ErrorCode::INVALID_ARG_VALUE_RangeError.fmt(global, format_args!("x"));
        this.put(global, "twin", twin);
        assert_eq!(
            eval(global, "`${twin}|${twin instanceof RangeError}`")
                .as_string()
                .as_deref(),
            Some("RangeError [ERR_INVALID_ARG_VALUE]: x|true")
        );
    });
}

#[test]
fn uncatchable_errors() {
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        this.put(
            global,
            "terminate",
            JSFunction::create(global, "terminate", terminate, 0),
        );
        // Boa unwinds every frame at its recursion limit, as JavaScriptCore does for a stack overflow; Rust sees
        // the RangeError JavaScriptCore throws then.
        let error = global
            .evaluate_script("function f() { f() } try { f() } catch { 'caught' }")
            .unwrap_err();
        let scope = TopExceptionScope::new(global);
        let peeked = scope
            .exception()
            .expect("an uncatchable error is an exception too");
        assert!(scope.has_exception());
        assert_eq!(
            string_of(global, &peeked, "message").as_deref(),
            Some("Maximum call stack size exceeded.")
        );
        let taken = global.take_exception(error);
        assert_eq!(
            string_of(global, &taken, "name").as_deref(),
            Some("RangeError")
        );
        assert!(!global.has_exception());
        assert_eq!(
            eval(global, "'runs again'").as_string().as_deref(),
            Some("runs again")
        );

        // A termination is not something script can catch either.
        let error = global
            .evaluate_script("try { terminate() } catch { 'caught' }")
            .unwrap_err();
        assert!(!global.clear_catchable_exception(), "it is not catchable");
        let taken = global.take_exception(error);
        assert_eq!(
            string_of(global, &taken, "message").as_deref(),
            Some("the JavaScript thread was terminated")
        );
    });
}

#[test]
fn roots() {
    JSGlobalObject::with_new(|global| {
        let mut strong = Strong::create(eval(global, "({ kept: 'yes' })"), global);
        let object = JSObject::create(global);
        let weak = Weak::create(object.to_js(), global);
        let weak_primitive = Weak::create(JSValue::js_number(3.0), global);
        assert!(weak.get().is_some());
        drop(object);
        eval(global, "for (let i = 0; i < 1000; i++) ({ garbage: [i] })");
        boa_gc::force_collect();
        assert!(weak.get().is_none(), "nothing holds the object");
        assert_eq!(weak_primitive.get().and_then(|v| v.get_number()), Some(3.0));
        // Only the Strong holds its object, across a collection.
        let weak_of_strong = Weak::create(strong.get(), global);
        boa_gc::force_collect();
        assert_eq!(
            string_of(global, &strong.get(), "kept").as_deref(),
            Some("yes")
        );
        assert!(weak_of_strong.get().is_some());
        assert!(strong.has());
        let value = strong.swap();
        assert!(!strong.has() && strong.get().is_undefined());
        strong.set(global, value);
        assert!(strong.try_get().is_some());
        strong.clear();
        assert!(!strong.has());
        boa_gc::force_collect();
        assert!(
            weak_of_strong.get().is_none(),
            "cleared, the Strong holds nothing"
        );
    });
}

#[test]
fn modules() {
    JSGlobalObject::with_new(|global| {
        global.register_native_module("bun:answer", |global| {
            Ok(vec![
                ("answer", JSValue::js_number(42.0)),
                (
                    "object",
                    JSObject::create_from(global, &[("ok", JSValue::js_boolean(true))])?.to_js(),
                ),
            ])
        });
        let module = JSModule::parse(
            global,
            "import { answer, object } from 'bun:answer'; export const doubled = answer * 2; export const ok = object.ok;",
            None,
        )
        .unwrap();
        let namespace = module.evaluate(global).unwrap();
        assert_eq!(
            namespace
                .get(global, "doubled")
                .unwrap()
                .unwrap()
                .get_number(),
            Some(84.0)
        );
        assert!(namespace.get(global, "ok").unwrap().unwrap().as_boolean());
        // Made once per realm.
        let again = global.native_module("bun:answer").unwrap().unwrap();
        let first = again.namespace(global);
        let second = global
            .native_module("bun:answer")
            .unwrap()
            .unwrap()
            .namespace(global);
        assert!(first.is_same_value(second, global).unwrap());
        assert!(global.native_module("bun:none").unwrap().is_none());

        let synthetic = JSModule::synthetic(global, vec![("value", JSValue::from_utf8(b"s"))]);
        let namespace = synthetic.evaluate(global).unwrap();
        assert_eq!(string_of(global, &namespace, "value").as_deref(), Some("s"));

        let missing = JSModule::parse(global, "import 'nowhere';", None).unwrap();
        assert_eq!(missing.evaluate(global).map(drop), Err(JsError::Thrown));
        global.clear_exception();
        let throws = JSModule::parse(global, "throw new Error('top level')", None).unwrap();
        let error = throws.evaluate(global).unwrap_err();
        let exception = global.take_exception(error);
        assert_eq!(
            string_of(global, &exception, "message").as_deref(),
            Some("top level")
        );
    });
}

#[test]
fn microtasks() {
    JSGlobalObject::with_new(|global| {
        eval(
            global,
            "globalThis.order = []; Promise.resolve().then(() => order.push('then')); order.push('sync')",
        );
        global.drain_microtasks().unwrap();
        assert_eq!(
            eval(global, "order.join()").as_string().as_deref(),
            Some("sync,then")
        );
    });
}

/// What Node's validators ask of a value: `Array.isArray` through a proxy, buffer views and their lengths, a
/// symbol's description.
#[test]
fn arrays_buffers_and_symbols() {
    JSGlobalObject::with_new(|global| {
        let proxy = eval(global, "new Proxy([1, 2], {})");
        assert!(!proxy.is_array() && proxy.is_array_abstract(global).unwrap());
        assert!(!eval(global, "({ length: 0 })").is_array_abstract(global).unwrap());
        let revoked = eval(global, "(() => { const p = Proxy.revocable([], {}); p.revoke(); return p.proxy })()");
        let thrown = revoked.is_array_abstract(global).expect_err("a revoked proxy throws");
        assert_eq!(global.take_exception(thrown).get(global, "name").unwrap().and_then(|n| n.as_string()).as_deref(), Some("TypeError"));

        let typed = eval(global, "new Uint16Array(3)");
        assert!(typed.is_array_buffer_view() && eval(global, "new DataView(new ArrayBuffer(4))").is_array_buffer_view());
        assert!(!eval(global, "new ArrayBuffer(4)").is_array_buffer_view());
        assert_eq!(typed.array_buffer_length(global).unwrap(), Some(3));
        assert_eq!(eval(global, "new DataView(new ArrayBuffer(4), 1)").array_buffer_length(global).unwrap(), Some(3));
        assert_eq!(eval(global, "new ArrayBuffer(5)").array_buffer_length(global).unwrap(), Some(5));
        assert_eq!(eval(global, "[1]").array_buffer_length(global).unwrap(), None);

        assert_eq!(eval(global, "Symbol('s')").symbol_description().as_deref(), Some("s"));
        assert_eq!(eval(global, "Symbol()").symbol_description(), None);
        assert_eq!(JSValue::js_number(1.0).symbol_description(), None);
    });
}

/// The async context (`$asyncContext`): once tracking is on, a promise job runs in the context it was queued in,
/// `await` included, and puts back the one in place after it.
#[test]
fn promise_jobs_carry_the_async_context() {
    fn context_now(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
        Ok(global.async_context())
    }
    JSGlobalObject::with_new(|global| {
        global.to_js_value().put(global, "contextNow", JSFunction::create(global, "contextNow", context_now, 0));
        eval(global, "globalThis.seen = []");

        // Untracked: a job sees whatever is in place when it runs.
        global.set_async_context(JSValue::from_utf8(b"before"));
        eval(global, "Promise.resolve().then(() => seen.push(contextNow()))");
        global.set_async_context(JSValue::from_utf8(b"later"));
        global.drain_microtasks().unwrap();

        global.enable_async_context_tracking();
        assert!(global.is_async_context_tracking_enabled());
        global.set_async_context(JSValue::from_utf8(b"queued"));
        eval(
            global,
            "Promise.resolve().then(() => seen.push(contextNow())); (async () => { await null; seen.push(contextNow()); await 0; seen.push(contextNow()) })()",
        );
        global.set_async_context(JSValue::from_utf8(b"outside"));
        global.drain_microtasks().unwrap();
        assert_eq!(eval(global, "seen.join()").as_string().as_deref(), Some("later,queued,queued,queued"));
        assert_eq!(global.async_context().as_string().as_deref(), Some("outside"));
    });
}

#[test]
fn internal_fields() {
    JSGlobalObject::with_new(|global| {
        let object = JSObject::create(global);
        assert!(object.get_internal_field(global, 1).unwrap().is_undefined());
        object
            .put_internal_field(global, 1, JSValue::js_number(7.0))
            .unwrap();
        assert_eq!(
            object.get_internal_field(global, 1).unwrap().get_number(),
            Some(7.0)
        );
        assert!(object.get_internal_field(global, 0).unwrap().is_undefined());
        // Script reads the same fields, through the realm's WeakMap.
        global
            .to_js_value()
            .put(global, "fields", global.internal_fields());
        global.to_js_value().put(global, "object", object.to_js());
        let seen = eval(global, "fields.get(object)[1]");
        assert_eq!(seen.get_number(), Some(7.0));
        assert!(eval(global, "fields.get({})").is_undefined());
    });
}

#[test]
fn settled_promises() {
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        this.put(
            global,
            "resolved",
            JSPromise::resolved_promise_value(global, JSValue::js_number(1.0)),
        );
        this.put(
            global,
            "rejected",
            JSPromise::rejected_promise_value(global, JSValue::from_utf8(b"no")),
        );
        eval(
            global,
            "globalThis.out = []; resolved.then(v => out.push(v)); rejected.catch(e => out.push(e));",
        );
        global.drain_microtasks().unwrap();
        assert_eq!(
            eval(global, "out.join()").as_string().as_deref(),
            Some("1,no")
        );
    });
}

/// A class whose constructor finishes on the instance: `initialize` sees the object that `new` returns.
#[derive(boa_gc::Trace, boa_gc::Finalize)]
struct SelfAware {
    me: JSValue,
}

impl bun_js::JsClass for SelfAware {
    const NAME: &'static str = "SelfAware";

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<SelfAware> {
        Ok(SelfAware {
            me: JSValue::UNDEFINED,
        })
    }

    fn initialize(
        this: &bun_js::ClassRef<SelfAware>,
        global: &JSGlobalObject,
        frame: &CallFrame<'_>,
    ) -> JsResult<()> {
        if frame.argument(0).is_truthy() {
            return Err(global.throw_type_error(format_args!("initialize threw")));
        }
        this.borrow_mut(global)?.me = this.to_js();
        Ok(())
    }

    fn define(class: &mut bun_js::class::ClassBuilder<'_, Self>) -> JsResult<()> {
        class.getter("me", bun_js::class::Attributes::DEFAULT, |this, global| {
            Ok(this.borrow(global)?.me.clone())
        })
    }
}

#[test]
fn initialize_sees_the_instance() {
    JSGlobalObject::with_new(|global| {
        let constructor = bun_js::class::constructor::<SelfAware>(global).unwrap();
        global.to_js_value().put(global, "SelfAware", constructor);
        assert_eq!(
            eval(global, "const s = new SelfAware(); String(s.me === s)").as_string().as_deref(),
            Some("true")
        );
        assert_eq!(
            eval(global, "try { new SelfAware(1); 'no' } catch (e) { e.message }").as_string().as_deref(),
            Some("initialize threw")
        );
    });
}

#[test]
fn array_buffers_and_string_units() {
    use bun_js::{StringUnits, TypedArrayType};
    JSGlobalObject::with_new(|global| {
        let view = eval(global, "globalThis.u16 = new Uint16Array([1, 0x203]).subarray(1); u16");
        let bytes = view.as_array_buffer(global).unwrap().unwrap();
        assert_eq!(bytes.typed_array_type(), TypedArrayType::Uint16Array);
        assert_eq!((bytes.byte_offset(), bytes.byte_len(), bytes.len()), (2, 2, 1));
        assert_eq!(bytes.with_bytes(<[u8]>::to_vec), [3, 2]);
        bytes.with_bytes_mut(|b| b[0] = 9);
        assert_eq!(eval(global, "u16[0]").as_number(), f64::from(0x209));

        let data = eval(global, "new DataView(new ArrayBuffer(4), 1, 2)");
        let data = data.as_array_buffer(global).unwrap().unwrap();
        assert_eq!((data.typed_array_type(), data.byte_len()), (TypedArrayType::DataView, 2));
        let buffer = eval(global, "u16.buffer").as_array_buffer(global).unwrap().unwrap();
        assert_eq!((buffer.typed_array_type(), buffer.byte_len()), (TypedArrayType::ArrayBuffer, 4));
        assert!(buffer.same_buffer(&bytes) && !buffer.same_buffer(&data));
        let shared = eval(global, "globalThis.shared = new Uint8Array(new SharedArrayBuffer(4), 1, 2); shared");
        let shared = shared.as_array_buffer(global).unwrap().unwrap();
        assert!(shared.is_shared());
        shared.with_bytes_mut(|b| b.copy_from_slice(&[7, 8]));
        assert_eq!(eval(global, "[...new Uint8Array(shared.buffer)].join()").as_string().as_deref(), Some("0,7,8,0"));

        let detached = eval(global, "const d = new Uint8Array(4); d.buffer.transfer(); d");
        let detached = detached.as_array_buffer(global).unwrap().unwrap();
        assert!(detached.is_detached() && detached.with_bytes(<[u8]>::is_empty));
        assert!(eval(global, "({})").as_array_buffer(global).unwrap().is_none());

        let made = JSValue::create_uint8_array(global, &[1, 2, 3]).unwrap();
        let made = made.as_array_buffer(global).unwrap().unwrap();
        assert_eq!(made.with_bytes(<[u8]>::to_vec), [1, 2, 3]);

        let units = |value: &JSValue| {
            value.with_string_units(|units| match units {
                StringUnits::Latin1(units) => format!("latin1 {units:?}"),
                StringUnits::Utf16(units) => format!("utf16 {units:?}"),
            })
        };
        assert_eq!(units(&JSValue::from_latin1(&[0xe9])).as_deref(), Some("latin1 [233]"));
        assert_eq!(JSValue::from_latin1(&[0xe9]).as_string().as_deref(), Some("é"));
        assert_eq!(units(&eval(global, "'€'")).as_deref(), Some("utf16 [8364]"));
        assert_eq!(units(&JSValue::js_number(1.0)), None);
    });
}
