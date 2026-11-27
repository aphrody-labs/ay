//! What node's APIs make and read: pending promises, values cached per realm, BigInts, `new`, dates, the bytes of
//! buffers and views.

use bun_js::{JSGlobalObject, JSPromise, JSValue};

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
fn pending_promises_settle_once() {
    JSGlobalObject::with_new(|global| {
        let resolved = JSPromise::create(global);
        let rejected = JSPromise::create(global);
        global.to_js_value().put(global, "a", resolved.promise());
        global.to_js_value().put(global, "b", rejected.promise());
        eval(global, "var got = []; a.then(v => got.push(v)); b.catch(e => got.push(e.message));");
        resolved.resolve(global, JSValue::js_number(42.0)).unwrap();
        let error = global.create_error_instance(format_args!("nope"));
        rejected.reject(global, error).unwrap();
        global.drain_microtasks().unwrap();
        assert_eq!(text(global, "JSON.stringify(got)"), r#"[42,"nope"]"#);
    });
}

#[test]
fn cached_values_are_made_once_per_realm() {
    struct Key;
    JSGlobalObject::with_new(|global| {
        let made = std::cell::Cell::new(0);
        let make = |global: &JSGlobalObject| {
            made.set(made.get() + 1);
            global.evaluate_script("({})")
        };
        let a = global.cached::<Key>(make).unwrap();
        let b = global.cached::<Key>(make).unwrap();
        assert!(a.is_strict_equal(b, global).unwrap());
        assert_eq!(made.get(), 1);
    });
    JSGlobalObject::with_new(|global| {
        assert!(global.cached::<Key>(|g| Err(g.throw(format_args!("no")))).is_err());
        global.clear_exception();
        assert!(global.cached::<Key>(|g| g.evaluate_script("1")).is_ok());
    });
}

#[test]
fn bigints_constructions_and_dates() {
    JSGlobalObject::with_new(|global| {
        assert_eq!(JSValue::from_i128(-(1i128 << 70)).to_utf8(global).unwrap(), "-1180591620717411303424");
        assert!(JSValue::from_i128(1).is_big_int());
        let date = eval(global, "Date").construct(global, &[JSValue::js_number(1500.0)]).unwrap();
        assert_eq!(date.as_date_ms(global).unwrap(), Some(1500.0));
        let invalid = eval(global, "new Date(NaN)").as_date_ms(global).unwrap();
        assert!(invalid.is_some_and(f64::is_nan));
        assert_eq!(JSValue::js_number(1.0).as_date_ms(global).unwrap(), None);
        assert!(JSValue::js_number(1.0).construct(global, &[]).is_err());
        global.clear_exception();
    });
}

#[test]
fn bytes_of_buffers_and_views() {
    JSGlobalObject::with_new(|global| {
        let buffer = eval(global, "new Uint8Array([1, 2, 3, 4]).buffer");
        assert_eq!(buffer.array_buffer_or_view_bytes(global).unwrap(), Some(vec![1, 2, 3, 4]));
        assert!(buffer.is_uint8_array_data_view_or_array_buffer());
        let view = eval(global, "new DataView(new Uint8Array([1, 2, 3, 4]).buffer, 1, 2)");
        assert_eq!(view.array_buffer_or_view_bytes(global).unwrap(), Some(vec![2, 3]));
        assert!(view.is_uint8_array_data_view_or_array_buffer());
        assert!(!eval(global, "new Uint16Array(2)").is_uint8_array_data_view_or_array_buffer());
        assert_eq!(eval(global, "'text'").array_buffer_or_view_bytes(global).unwrap(), None);

        // Into a view of part of a buffer: what fits, from the offset.
        let target = eval(global, "var whole = new Uint8Array(6); new Uint8Array(whole.buffer, 1, 4)");
        assert_eq!(target.write_array_buffer_view(global, 1, &[9, 8, 7, 6, 5]).unwrap(), 3);
        assert_eq!(text(global, "whole.join()"), "0,0,9,8,7,0");
        assert_eq!(target.write_array_buffer_view(global, 10, &[1]).unwrap(), 0);
    });
}
