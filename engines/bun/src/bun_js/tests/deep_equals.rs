//! `JSValue::deep_equals_with`, `Bun.deepEquals` on Boa. Each expected value is what JavaScriptCore Bun's
//! `Bun.deepEquals(a, b, strict)` answers for the same pair.

use bun_js::{DeepEqualsMode, JSGlobalObject};

/// `(the two values, strict, Bun.deepEquals)`.
const CASES: &[(&str, bool, bool)] = &[
        ("1, 1", false, true),
        ("1, '1'", false, false),
        ("NaN, NaN", false, true),
        ("0, -0", false, false),
        ("{a:1}, {a:1}", false, true),
        ("{a:1}, {a:2}", false, false),
        ("{a:1, b:undefined}, {a:1}", false, true),
        ("{a:1, b:undefined}, {a:1}", true, false),
        ("{a:1}, {a:1, b:undefined}", true, false),
        ("[1,2,3], [1,2,3]", false, true),
        ("[1,2], [1,2,3]", false, false),
        ("[1,,3], [1,undefined,3]", false, true),
        ("[1,,3], [1,undefined,3]", true, false),
        ("[1,2], [1,2,undefined]", false, true),
        ("[1,2], [1,2,undefined]", true, false),
        ("new Map([[1,{a:1}]]), new Map([[1,{a:1}]])", false, true),
        ("new Map([[{k:1},1]]), new Map([[{k:1},1]])", false, true),
        ("new Map([[1,1]]), new Map([[1,2]])", false, false),
        ("new Set([1,{a:1}]), new Set([1,{a:1}])", false, true),
        ("new Set([1]), new Set([2])", false, false),
        ("new Date(0), new Date(0)", false, true),
        ("new Date(NaN), new Date(NaN)", false, false),
        ("new Date(0), new Date(1)", false, false),
        ("/a/g, /a/g", false, true),
        ("/a/g, /a/i", false, false),
        ("new Error('x'), new Error('x')", false, true),
        ("new Error('x'), new TypeError('x')", false, false),
        ("new Error('x'), new Error('y')", false, false),
        ("new Error('x', {cause: 1}), new Error('x')", true, false),
        ("new Error('x', {cause: 1}), new Error('x')", false, false),
        ("new Uint8Array([1,2]), new Uint8Array([1,2])", false, true),
        ("new Uint8Array([1,2]), new Int8Array([1,2])", false, false),
        ("new Float64Array([0]), new Float64Array([-0])", false, true),
        ("new Float64Array([0]), new Float64Array([-0])", true, false),
        ("new Float64Array([NaN]), new Float64Array([NaN])", false, false),
        ("new Float64Array([NaN]), new Float64Array([NaN])", true, true),
        ("new ArrayBuffer(2), new ArrayBuffer(2)", false, true),
        ("new ArrayBuffer(2), new ArrayBuffer(3)", false, false),
        ("new String('a'), new String('a')", false, true),
        ("new String('a'), new String('b')", false, false),
        ("new Number(1), new Number(1)", false, true),
        ("new Number(1), new Number(2)", false, false),
        ("new Number(1), 1", false, false),
        ("new Boolean(true), new Number(1)", false, false),
        ("Object(1n), Object(1n)", false, true),
        ("Object(Symbol.iterator), Object(Symbol.iterator)", false, true),
        ("() => 1, () => 1", false, false),
        ("(() => { class A { constructor(){this.x=1} }; return [new A] })()[0], {x:1}", false, true),
        ("(() => { class A { constructor(){this.x=1} }; return [new A] })()[0], {x:1}", true, false),
        ("Object.create(null), {}", true, true),
        ("Object.create(null), {}", false, true),
        ("(() => { const a = {}; a.self = a; return a })(), (() => { const b = {}; b.self = b; return b })()", false, true),
        ("{[Symbol.for('s')]: 1}, {[Symbol.for('s')]: 1}", false, true),
        ("{[Symbol.for('s')]: 1}, {[Symbol.for('s')]: 2}", false, false),
        ("Object.assign([1], {[Symbol.for('s')]: 1}), [1]", false, false),
        ("{0: 'a', 1: 'b'}, {0: 'a', 1: 'b'}", false, true),
        ("{0: 'a'}, {0: 'b'}", false, false),
        ("Object.create({a: 1}), {}", false, true),
        ("new DataView(new ArrayBuffer(1)), new DataView(new ArrayBuffer(2))", false, true),
        ("{a: [1, {b: new Map([['c', new Set([1])]])}]}, {a: [1, {b: new Map([['c', new Set([1])]])}]}", false, true),
        ("Object.assign(new Uint8Array(1), {x: 1}), new Uint8Array(1)", true, false),
        ("Object.assign(new Uint8Array(1), {x: 1}), new Uint8Array(1)", false, true),
];

#[test]
fn as_javascriptcore_answers() {
    JSGlobalObject::with_new(|global| {
        let mut failures = Vec::new();
        for &(values, strict, expected) in CASES {
            let pair = global
                .evaluate_script(&format!("[{values}]"))
                .unwrap_or_else(|_| panic!("{values}"));
            let (a, b) = (pair.get_index(global, 0).unwrap(), pair.get_index(global, 1).unwrap());
            let mode = DeepEqualsMode {
                strict,
                skip_prototype_identity: false,
            };
            match a.deep_equals_with(&b, global, mode) {
                Ok(actual) if actual == expected => {}
                Ok(actual) => failures.push(format!("{values} (strict: {strict}): {actual}, not {expected}")),
                Err(error) => failures.push(format!(
                    "{values} (strict: {strict}): threw {:?}",
                    global.take_exception(error).to_utf8(global)
                )),
            }
        }
        assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
    });
}

#[test]
fn skip_prototype_identity() {
    JSGlobalObject::with_new(|global| {
        let pair = global
            .evaluate_script("class A { constructor() { this.x = 1 } }; [new A, { x: 1 }]")
            .unwrap();
        let (a, b) = (pair.get_index(global, 0).unwrap(), pair.get_index(global, 1).unwrap());
        assert!(!a.strict_deep_equals(&b, global).unwrap());
        let mode = DeepEqualsMode {
            strict: true,
            skip_prototype_identity: true,
        };
        assert!(a.deep_equals_with(&b, global, mode).unwrap());
        assert!(a.deep_equals(&b, global).unwrap());
    });
}

#[test]
fn a_getter_that_throws() {
    JSGlobalObject::with_new(|global| {
        let pair = global
            .evaluate_script("[{ get a() { throw new Error('read') } }, { a: 1 }]")
            .unwrap();
        let (a, b) = (pair.get_index(global, 0).unwrap(), pair.get_index(global, 1).unwrap());
        let error = a.deep_equals(&b, global).unwrap_err();
        let thrown = global.take_exception(error);
        assert_eq!(
            thrown.get(global, "message").unwrap().unwrap().as_string().as_deref(),
            Some("read")
        );
    });
}
