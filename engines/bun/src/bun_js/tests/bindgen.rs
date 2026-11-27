//! What src/codegen/bindgen-js.ts writes from tests/fixture.bind.ts, called from JavaScript.

use boa_gc::{Finalize, Trace};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsResult, Weak};

mod fixture {
    use super::*;
    use bun_js::bindgen::{self, JsHeap, JsLocal, JsScope};

    pub fn add(a: i32, b: i32) -> i32 {
        a.wrapping_add(b)
    }

    pub fn greet<'a>(
        heap: &JsHeap<'a>,
        name: &'a [u8],
        punctuation: &'a [u8],
    ) -> bindgen::Result<'a, Vec<u8>> {
        if name.is_empty() {
            return Err(heap.type_error(format_args!("greet: the name is empty")));
        }
        Ok([b"hello ", name, punctuation].concat())
    }

    pub fn call<'a>(
        cx: &mut JsScope<'a>,
        callback: JsLocal<'a>,
        flag: Option<bool>,
    ) -> bindgen::Result<'a, JSValue> {
        let global = cx.global();
        let argument = JSValue::js_boolean(flag.unwrap_or(false));
        Ok(callback
            .value()
            .call(global, JSValue::UNDEFINED, &[argument])?)
    }

    pub fn join_all<'a>(
        _: &JsHeap<'a>,
        separator: &'a [u8],
        parts: &[&'a [u8]],
    ) -> bindgen::Result<'a, Vec<u8>> {
        Ok(parts.join(separator))
    }

    pub fn split<'a>(
        _: &JsHeap<'a>,
        text: &'a [u8],
        items: &[JsLocal<'a>],
    ) -> bindgen::Result<'a, crate::generated::fixture::Split<'a>> {
        let end = bun_core::strings::index_of_char_usize(text, b' ').unwrap_or(text.len());
        Ok(crate::generated::fixture::Split {
            head: &text[..end],
            count: items.len() as u32,
            last: items.last().cloned().unwrap_or(JsLocal::UNDEFINED),
            done: (end == text.len()).then_some(true),
        })
    }

    pub fn describe_shape<'a>(
        _: &JsHeap<'a>,
        shape: Option<crate::generated::fixture::ShapeOrName<'a>>,
    ) -> bindgen::Result<'a, String> {
        use crate::generated::fixture::ShapeOrName;
        Ok(match shape {
            None => "nothing".to_owned(),
            Some(ShapeOrName::Name(name)) => format!("named {}", String::from_utf8_lossy(name)),
            Some(ShapeOrName::Shape(shape)) => format!(
                "{} sides, {}, {}",
                shape.sides,
                shape.label.map_or("unlabelled".into(), String::from_utf8_lossy),
                if shape.filled { "filled" } else { "empty" }
            ),
        })
    }

    pub fn describe_wide(wide: crate::generated::fixture::Wide) -> String {
        use crate::generated::fixture::Wide;
        match wide {
            Wide::Number(number) => format!("number {number}"),
            Wide::Bigint(bigint) => format!("bigint {bigint}"),
        }
    }

    pub fn same<'a>(_: &JsHeap<'a>, value: JsLocal<'a>) -> JsLocal<'a> {
        value
    }

    pub fn half(value: f64) -> f64 {
        value / 2.0
    }

    pub fn is_truthy(value: bool) -> bool {
        value
    }

    #[derive(Trace, Finalize)]
    pub struct Counter {
        pub count: u32,
        pub label: String,
    }

    impl Counter {
        pub fn constructor(start: u32) -> Self {
            Counter {
                count: start,
                label: "counter".to_owned(),
            }
        }

        pub fn get_count(&self) -> u32 {
            self.count
        }

        pub fn get_label(&self) -> String {
            self.label.clone()
        }

        pub fn set_label(&mut self, value: &[u8]) {
            self.label = String::from_utf8_lossy(value).into_owned();
        }

        pub fn increment(&mut self, by: u32) -> u32 {
            self.count += by;
            self.count
        }

        pub fn describe<'a>(&self, heap: &JsHeap<'a>) -> bindgen::Result<'a, String> {
            if self.label.is_empty() {
                return Err(heap.range_error(format_args!("no label")));
            }
            Ok(format!("{} at {}", self.label, self.count))
        }

        pub fn zero() -> u32 {
            0
        }

        pub fn peek(&self) -> u32 {
            self.count
        }
    }

    pub fn count_arguments(args: &[JSValue]) -> u32 {
        args.len() as u32
    }

    #[derive(Trace, Finalize)]
    pub struct Tally {
        pub base: f64,
    }

    impl Tally {
        pub fn sum(&self, cx: &mut JsScope<'_>, values: &[JSValue]) -> JsResult<f64> {
            let global = cx.global();
            let mut total = self.base;
            for value in values {
                total += value.to_number(global)?;
            }
            Ok(total)
        }
    }

    pub fn tally(_: &mut JsScope<'_>) -> Tally {
        Tally { base: 0.5 }
    }
}

mod generated {
    include!(concat!(
        env!("BUN_CODEGEN_DIR"),
        "/generated_bun_js_bun_js.rs"
    ));
}

/// A realm with the fixture's exports on `globalThis`.
fn realm(f: impl FnOnce(&JSGlobalObject)) {
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        let exports = generated::fixture::exports(global).expect("exports");
        for (name, value) in exports
            .into_iter()
            .chain(generated::fixture::namespaces(global).expect("namespaces"))
        {
            this.put(global, name, value);
        }
        f(global)
    })
}

/// What `source` evaluates to, as a string; an exception it throws is a panic with its message.
fn eval(global: &JSGlobalObject, source: &str) -> String {
    match global.evaluate_script(source) {
        Ok(value) => value.to_utf8(global).unwrap(),
        Err(error) => {
            let exception = global.take_exception(error);
            panic!("{source}: threw {}", exception.to_utf8(global).unwrap())
        }
    }
}

/// `name: message (code)` of what `source` throws.
fn thrown(global: &JSGlobalObject, source: &str) -> String {
    eval(
        global,
        &format!(
            "try {{ {source}; 'no exception' }} catch (e) {{ `${{e.name}}: ${{e.message}} (${{e.code}})` }}"
        ),
    )
}

#[test]
fn functions_convert_their_arguments() {
    realm(|global| {
        assert_eq!(eval(global, "add(2, '3')"), "5");
        assert_eq!(eval(global, "add.length + ':' + add.name"), "2:add");
        // `.loose` is ToNumber, then the strict conversion: it does not truncate.
        assert_eq!(eval(global, "add(1, '2.0')"), "3");
        assert_eq!(
            thrown(global, "add(1, NaN)"),
            "RangeError: b must be an integer (received NaN) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "add(1, 0.5)"),
            "RangeError: b must be an integer (received 0.5) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "add()"),
            "TypeError: The \"a\" argument must be specified (ERR_MISSING_ARGS)"
        );
        assert_eq!(
            eval(
                global,
                "try { add() } catch (e) { String(e) + ':' + Object.hasOwn(e, 'code') + ':' + (e instanceof TypeError) }"
            ),
            "TypeError [ERR_MISSING_ARGS]: The \"a\" argument must be specified:false:true"
        );
        assert_eq!(
            thrown(global, "add('1')"),
            "TypeError: a must be a number (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(
            thrown(global, "add(1.5)"),
            "RangeError: a must be an integer (received 1.5) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "add(Infinity)"),
            "RangeError: a must be an integer (received Infinity) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "add(2 ** 31)"),
            "RangeError: a must be in the range [-2147483648, 2147483647] (received 2147483648) (ERR_OUT_OF_RANGE)"
        );
        // A property of Object.prototype changes nothing.
        assert_eq!(
            thrown(
                global,
                "Object.defineProperty(Object.prototype, 'code', { value: 1, configurable: true }); try { add() } finally { delete Object.prototype.code }"
            ),
            "TypeError: The \"a\" argument must be specified (ERR_MISSING_ARGS)"
        );
    });
}

#[test]
fn strings_defaults_and_errors_of_the_body() {
    realm(|global| {
        assert_eq!(eval(global, "greet('bün')"), "hello bün!");
        assert_eq!(eval(global, "greet('bun', 1)"), "hello bun1");
        assert_eq!(eval(global, "greet(new String('obj'), '?')"), "hello obj?");
        assert_eq!(
            thrown(global, "greet({})"),
            "TypeError: name must be a string (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(
            thrown(global, "greet('')"),
            "TypeError: greet: the name is empty (undefined)"
        );
    });
}

#[test]
fn a_body_that_runs_javascript() {
    realm(|global| {
        assert_eq!(eval(global, "call(flag => `got ${flag}`)"), "got false");
        assert_eq!(
            eval(global, "call(flag => `got ${flag}`, true)"),
            "got true"
        );
        assert_eq!(
            thrown(global, "call(() => { throw new SyntaxError('inner') })"),
            "SyntaxError: inner (undefined)"
        );
        assert_eq!(
            thrown(global, "call(1)"),
            "TypeError: number is not a function (undefined)"
        );
        assert_eq!(
            thrown(global, "call(() => 0, 1)"),
            "TypeError: flag must be a boolean (ERR_INVALID_ARG_TYPE)"
        );
        assert!(!global.has_exception());
    });
}

#[test]
fn namespaces() {
    realm(|global| {
        fn make(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
            Ok(generated::fixture::create_math(global))
        }
        global.to_js_value().put(
            global,
            "__make",
            bun_js::JSFunction::create(global, "__make", make, 0),
        );
        assert_eq!(eval(global, "math.half(3)"), "1.5");
        assert_eq!(eval(global, "half(5) + ':' + math.half.name"), "2.5:half");
        assert_eq!(
            eval(global, "[math.isTruthy(''), math.isTruthy('x')].join()"),
            "false,true"
        );
        assert_eq!(eval(global, "Object.keys(math).join()"), "half,isTruthy");
        // Built without [[Set]]: a setter of Object.prototype does not run.
        assert_eq!(
            eval(
                global,
                "let ran = false; Object.defineProperty(Object.prototype, 'half', { set() { ran = true }, configurable: true }); const m = globalThis.__make(); delete Object.prototype.half; ran + ':' + typeof m.half"
            ),
            "false:function"
        );
    });
}

#[test]
fn rest_string_objects_and_the_errors_of_validators() {
    realm(|global| {
        assert_eq!(eval(global, "joinAll('-', 'a', new String('b'), 'c')"), "a-b-c");
        assert_eq!(eval(global, "joinAll(new String('+'))"), "");
        // `namedAsBound`: its `name` is that of a bound function, and it prints as itself.
        assert_eq!(
            eval(global, "[joinAll.name, joinAll.length, String(joinAll)].join()"),
            "bound joinAll,1,function joinAll() { [native code] }"
        );
        assert_eq!(
            thrown(global, "joinAll('-', 'a', 1)"),
            "TypeError: The \"parts[1]\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(
            thrown(global, "joinAll('-', [])"),
            "TypeError: The \"parts[0]\" property must be of type string, got array (ERR_INVALID_ARG_TYPE)"
        );
        // `parts` is converted first. `separator` has the message of a literal conversion: no `validatorExpects`.
        assert_eq!(
            thrown(global, "joinAll(1, null)"),
            "TypeError: The \"parts[0]\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(
            thrown(global, "joinAll(1, 'a')"),
            "TypeError: separator must be a string (ERR_INVALID_ARG_TYPE)"
        );
        // Every String object is made a string, in order, before any argument is converted.
        assert_eq!(
            eval(
                global,
                "(() => { const log = []; const s = t => { const o = new String(t); o.toString = () => (log.push(t), t); return o }; try { joinAll(s('/'), 1, s('x')) } catch {} return log.join() })()"
            ),
            "/,x"
        );
        assert_eq!(
            thrown(
                global,
                "const bad = new String('x'); bad.toString = () => { throw new EvalError('to string') }; joinAll('-', bad)"
            ),
            "EvalError: to string (undefined)"
        );
        // Without `.allowStringObject`, a String object is not a string.
        assert_eq!(
            thrown(global, "split(new String('a b'))"),
            "TypeError: text must be a string (ERR_INVALID_ARG_TYPE)"
        );
        assert!(!global.has_exception());
    });
}

#[test]
fn a_dictionary_that_is_returned_and_any_values() {
    realm(|global| {
        assert_eq!(
            eval(global, "JSON.stringify(split('a b', 1, new String('z')))"),
            r#"{"head":"a","count":2,"last":"z"}"#
        );
        // `.optional` and absent: left out.
        assert_eq!(eval(global, "Object.keys(split('a b')).join()"), "head,count,last");
        assert_eq!(eval(global, "Object.keys(split('ab')).join()"), "head,count,last,done");
        assert_eq!(eval(global, "split('ab').done"), "true");
        // `b.any.stringObjectAsString`: a String object arrives as its string.
        assert_eq!(eval(global, "typeof split('a', new String('z')).last"), "string");
        assert_eq!(
            eval(global, "Object.getPrototypeOf(split('')) === Object.prototype"),
            "true"
        );
        // `b.any.returning` holds the result to nothing: a String object stays one.
        assert_eq!(eval(global, "typeof same(new String('q'))"), "object");
        assert_eq!(eval(global, "const o = {}; same(o) === o"), "true");
    });
}

#[test]
fn dictionaries_and_unions_that_are_taken() {
    realm(|global| {
        assert_eq!(eval(global, "describeShape()"), "nothing");
        assert_eq!(eval(global, "describeShape(null)"), "nothing");
        assert_eq!(eval(global, "describeShape('box')"), "named box");
        assert_eq!(eval(global, "describeShape(new String('box'))"), "named box");
        assert_eq!(eval(global, "describeShape({ sides: 3 })"), "3 sides, unlabelled, filled");
        // A second name is read where the first is undefined; `.default()` of `.nullable` takes null.
        assert_eq!(
            eval(global, "describeShape({ sides: 4, name: 'square', filled: null })"),
            "4 sides, square, filled"
        );
        assert_eq!(
            eval(global, "describeShape({ sides: 4, label: 'a', name: 'b', filled: 0 })"),
            "4 sides, a, empty"
        );
        // From a prototype, but not from Object.prototype.
        assert_eq!(
            eval(global, "describeShape(Object.create({ sides: 5 }))"),
            "5 sides, unlabelled, filled"
        );
        assert_eq!(
            thrown(
                global,
                "Object.prototype.sides = 6; try { describeShape({}) } finally { delete Object.prototype.sides }"
            ),
            "TypeError: Shape.sides is required (ERR_INVALID_ARG_TYPE)"
        );
        // Members are read in order, each once.
        assert_eq!(
            eval(
                global,
                "(() => { const read = []; const o = {}; for (const k of ['filled', 'name', 'label', 'sides']) Object.defineProperty(o, k, { get() { read.push(k); return k === 'sides' ? 1 : undefined } }); describeShape(o); return read.join() })()"
            ),
            "sides,label,name,filled"
        );
        assert_eq!(
            thrown(global, "describeShape(5)"),
            "TypeError: Shape must be an object (ERR_INVALID_ARG_TYPE)"
        );
        // Without `allowArray`, an array is not one.
        assert_eq!(
            thrown(global, "describeShape([])"),
            "TypeError: Shape must be an object (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(
            thrown(global, "describeShape({ sides: 1, label: 2 })"),
            "TypeError: Shape.label must be a string (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(eval(global, "describeWide(1.5)"), "number 1.5");
        assert_eq!(
            eval(global, "describeWide(2n ** 64n - 1n)"),
            "bigint 18446744073709551615"
        );
        assert_eq!(
            thrown(global, "describeWide(2n ** 64n)"),
            "RangeError: bigint must be in the range [0, 18446744073709551615] (received 18446744073709551616n) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "describeWide(-1n)"),
            "RangeError: bigint must be in the range [0, 18446744073709551615] (received -1n) (ERR_OUT_OF_RANGE)"
        );
        assert_eq!(
            thrown(global, "describeWide('1')"),
            "TypeError: The \"wide\" property must be of type number or bigint, got string (ERR_INVALID_ARG_TYPE)"
        );
        assert!(!global.has_exception());
    });
}

#[test]
fn a_method_keyed_by_a_private_name() {
    realm(|global| {
        assert_eq!(eval(global, "new Counter(7).__intrinsic__counterPeek()"), "7");
        assert_eq!(eval(global, "Counter.prototype.__intrinsic__counterPeek.name"), "__peek");
        let descriptor = "Object.getOwnPropertyDescriptor(Counter.prototype, '__intrinsic__counterPeek')";
        assert_eq!(
            eval(global, &format!("[{descriptor}.writable, {descriptor}.enumerable].join()")),
            "false,false"
        );
        assert_eq!(eval(global, "'__peek' in Counter.prototype"), "false");
    });
}

#[test]
fn natives_as_bindgen_answers_them() {
    realm(|global| {
        let native = |symbol| {
            generated::native(global, "fixture.bind.ts", symbol).map(|made| made.expect("made"))
        };
        assert!(native("nothing").is_none());
        assert!(generated::native(global, "other.bind.ts", "add").is_none());
        let add = native("add").expect("add");
        let five = add
            .call(
                global,
                JSValue::UNDEFINED,
                &[JSValue::js_number(2.0), JSValue::js_number(3.0)],
            )
            .unwrap();
        assert_eq!(five.get_number(), Some(5.0));
        // A function of a namespace by its key, and the namespace by its name.
        assert!(native("half").is_some_and(|half| half.is_callable()));
        assert!(
            native("math")
                .unwrap()
                .get(global, "isTruthy")
                .unwrap()
                .is_some()
        );
        // A class by its constructor.
        let counter = native("Counter").expect("Counter");
        global.to_js_value().put(global, "NativeCounter", counter);
        assert_eq!(
            eval(global, "new NativeCounter(4).count + ':' + (new NativeCounter() instanceof Counter)"),
            "4:true"
        );
        // A dictionary is a type, not a value.
        assert!(native("Split").is_none());
    });
}

#[test]
fn classes() {
    realm(|global| {
        assert_eq!(
            eval(
                global,
                "const c = new Counter(5); c.increment(); c.increment(2)"
            ),
            "8"
        );
        assert_eq!(eval(global, "c.count"), "8");
        assert_eq!(eval(global, "new Counter().count"), "0");
        assert_eq!(
            eval(global, "c.label = 'clicks'; c.describe()"),
            "clicks at 8"
        );
        assert_eq!(eval(global, "c.label = ''; c.label.length"), "0");
        assert_eq!(
            thrown(global, "c.describe()"),
            "RangeError: no label (undefined)"
        );
        assert_eq!(
            eval(
                global,
                "Counter.zero() + ':' + Counter.name + ':' + (c instanceof Counter)"
            ),
            "0:Counter:true"
        );
        assert_eq!(
            thrown(global, "Counter(1)"),
            "TypeError: Counter constructor cannot be invoked without 'new' (ERR_ILLEGAL_CONSTRUCTOR)"
        );
        assert_eq!(
            thrown(global, "Counter.prototype.increment.call({})"),
            "TypeError: Expected this to be instanceof Counter, but received an instance of Object (ERR_INVALID_THIS)"
        );
        assert_eq!(
            thrown(global, "Counter.prototype.increment.call(5)"),
            "TypeError: Expected this to be instanceof Counter, but received type number (5) (ERR_INVALID_THIS)"
        );
        assert_eq!(
            thrown(global, "Counter.prototype.increment.call()"),
            "TypeError: Expected this to be instanceof Counter (ERR_INVALID_THIS)"
        );
        assert_eq!(
            thrown(
                global,
                "Counter.prototype.increment.call(Object.create(null))"
            ),
            "TypeError: Expected this to be instanceof Counter, but received [Object: null prototype] {} (ERR_INVALID_THIS)"
        );
        assert_eq!(
            thrown(
                global,
                "Counter.prototype.increment.call({ constructor: 0, toString() { throw 1 } })"
            ),
            "TypeError: Expected this to be instanceof Counter, but received [Object] (ERR_INVALID_THIS)"
        );
        // An accessor is JavaScriptCore's `DOMAttribute`: its error is that one, without a code.
        assert_eq!(
            thrown(
                global,
                "Object.getOwnPropertyDescriptor(Counter.prototype, 'count').get.call({})"
            ),
            "TypeError: The Counter.count getter can only be used on instances of Counter (undefined)"
        );
        assert_eq!(
            thrown(
                global,
                "Object.getOwnPropertyDescriptor(Counter.prototype, 'label').set.call(1, 'x')"
            ),
            "TypeError: The Counter.label setter can only be used on instances of Counter (undefined)"
        );
        assert_eq!(
            eval(global, "Object.prototype.toString.call(new Counter())"),
            "[object Counter]"
        );
        // Describing the receiver reads its constructor's name: that exception wins.
        assert_eq!(
            thrown(
                global,
                "Counter.prototype.increment.call({ get constructor() { throw new EvalError('mine') } })"
            ),
            "EvalError: mine (undefined)"
        );
        // The bindgen's attributes for a class that is not `webIDL`: enumerable and writable, not configurable.
        assert_eq!(
            eval(
                global,
                "const d = Object.getOwnPropertyDescriptor(Counter.prototype, 'increment'); [d.enumerable, d.writable, d.configurable].join()"
            ),
            "true,true,false"
        );
        // `construct.length`, as C++: an argument with a default does not count.
        assert_eq!(eval(global, "Counter.length"), "0");
        assert_eq!(
            eval(
                global,
                "class Sub extends Counter { twice() { return this.increment(this.count) } }; new Sub(3).twice()"
            ),
            "6"
        );

        let made = bun_js::class::to_js(
            global,
            fixture::Counter {
                count: 41,
                label: "rust".into(),
            },
        )
        .unwrap();
        global.to_js_value().put(global, "made", made.clone());
        assert_eq!(eval(global, "made.increment()"), "42");
        let instance = made
            .as_class_ref::<fixture::Counter>()
            .expect("an instance of Counter");
        assert_eq!(instance.borrow(global).unwrap().count, 42);
        instance.borrow_mut(global).unwrap().label = "changed".into();
        assert_eq!(eval(global, "made.label"), "changed");
        assert!(
            JSValue::js_number(1.0)
                .as_class_ref::<fixture::Counter>()
                .is_none()
        );
    });
}

#[test]
fn script_that_reenters_an_instance() {
    realm(|global| {
        // The arguments are converted before the payload is borrowed: script they run may use the object.
        assert_eq!(
            eval(
                global,
                "const c = new Counter(1); const s = new String('x'); s.toString = () => (c.label + c.count); c.label = s; c.label"
            ),
            "counter1"
        );
        // A strict string reads nothing of a value that is not one.
        assert_eq!(
            thrown(
                global,
                "c.label = { get constructor() { throw new Error('read') } }"
            ),
            "TypeError: label must be a string (ERR_INVALID_ARG_TYPE)"
        );
        assert_eq!(eval(global, "c.count"), "1");

        // The schema keeps a generated member from running script while it borrows the payload mutably; one written
        // by hand can, and script that uses the payload meanwhile throws instead of aliasing it.
        let holder = bun_js::class::to_js(
            global,
            Holder {
                held: JSValue::NULL,
            },
        )
        .unwrap();
        global.to_js_value().put(global, "holder", holder);
        assert_eq!(
            thrown(global, "holder.hold(() => holder.held)"),
            "TypeError: This Holder is in use by a call that has not returned (undefined)"
        );
        assert_eq!(
            eval(global, "holder.hold(() => 'kept'); holder.held"),
            "kept"
        );
        assert!(!global.has_exception());
    });
}

/// A payload holding the JS object that holds it: the collector traces the payload, so the cycle is garbage.
#[derive(Trace, Finalize)]
struct Holder {
    held: JSValue,
}

impl bun_js::JsClass for Holder {
    const NAME: &'static str = "Holder";
    fn define(class: &mut bun_js::class::ClassBuilder<'_, Self>) -> JsResult<()> {
        class.getter(
            "held",
            bun_js::class::Attributes::DEFAULT,
            |this, global| Ok(this.borrow(global)?.held.clone()),
        )?;
        // Holds what the callback returns, which it calls with the payload borrowed mutably.
        class.method(
            "hold",
            1,
            bun_js::class::Attributes::DEFAULT,
            |this, global, frame| {
                let mut payload = this.borrow_mut(global)?;
                payload.held = frame.argument(0).call(global, JSValue::UNDEFINED, &[])?;
                Ok(JSValue::UNDEFINED)
            },
        )
    }
}

#[test]
fn variadic_arguments_and_values() {
    realm(|global| {
        assert_eq!(eval(global, "countArguments() + ':' + countArguments(1, 'a', {})"), "0:3");
        assert_eq!(eval(global, "countArguments.length"), "2");
        // `b.value`: `$bindgen` answers the instance, made each time it is asked for.
        let tally = generated::native(global, "fixture.bind.ts", "tally").expect("tally").unwrap();
        global.to_js_value().put(global, "t", tally);
        assert_eq!(eval(global, "t.sum(1, 2, '3') + ':' + t.sum()"), "6.5:0.5");
        assert_eq!(eval(global, "Object.prototype.toString.call(t)"), "[object Tally]");
        assert_eq!(
            thrown(global, "t.sum({ valueOf() { throw new RangeError('no') } })"),
            "RangeError: no (undefined)"
        );
        // Only Rust makes one.
        assert_eq!(
            thrown(global, "new t.constructor()"),
            "TypeError: Illegal constructor (ERR_ILLEGAL_CONSTRUCTOR)"
        );
    });
}

#[test]
fn payloads_are_traced() {
    JSGlobalObject::with_new(|global| {
        let weak = {
            let object = bun_js::JSObject::create(global);
            let holder = bun_js::class::to_js(
                global,
                Holder {
                    held: object.to_js(),
                },
            )
            .unwrap();
            object.put(global, "holder", holder.clone());
            let weak = Weak::create(holder.clone(), global);
            boa_gc::force_collect();
            let held = holder.get(global, "held").unwrap().unwrap();
            assert!(
                held.get(global, "holder").unwrap().is_some(),
                "alive while Rust holds it"
            );
            weak
        };
        boa_gc::force_collect();
        assert!(
            weak.get().is_none(),
            "the holder and its object are a cycle nothing else holds"
        );

        // Held by script only, it lives.
        global.to_js_value().put(
            global,
            "kept",
            bun_js::class::to_js(
                global,
                Holder {
                    held: JSValue::js_number(5.0),
                },
            )
            .unwrap(),
        );
        boa_gc::force_collect();
        assert_eq!(
            global.evaluate_script("kept.held").unwrap().get_number(),
            Some(5.0)
        );
    });
}
