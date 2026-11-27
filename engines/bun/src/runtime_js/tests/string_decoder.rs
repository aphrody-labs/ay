//! node:string_decoder's StringDecoder on bun_js, called from JavaScript. The expected values are what
//! JavaScriptCore Bun's StringDecoder answers for the same calls.

use bun_js::{JSGlobalObject, JSValue};
use bun_sema_standalone as _;

/// A realm with `StringDecoder`, the Rust class (not the module's function in front of it), and `r`, which
/// describes what a function answers or throws.
fn realm(f: impl FnOnce(&JSGlobalObject)) {
    JSGlobalObject::with_new(|global| {
        let constructor = bun_runtime_js::node::string_decoder::constructor(global).expect("the class");
        global.to_js_value().put(global, "StringDecoder", constructor);
        global
            .evaluate_script(
                "globalThis.r = f => { try { const v = f(); return typeof v === 'object' && v !== null ? `${Object.prototype.toString.call(v)}:${JSON.stringify(v)}` : `${typeof v}:${String(v)}` } catch (e) { return `throws ${e?.name}: ${e?.message} (${e?.code})` } }",
            )
            .expect("r");
        f(global)
    })
}

fn eval(global: &JSGlobalObject, source: &str) -> String {
    match global.evaluate_script(source) {
        Ok(value) => value.to_utf8(global).expect("a string"),
        Err(error) => format!("uncaught {:?}", global.take_exception(error).to_utf8(global)),
    }
}

/// Chunks written to a decoder of each encoding, then `end()`: each chunk's string, the end's, and the state left.
const SEQUENCES: &str = "[[\"utf8\",[[226,130],[172]],true],[\"utf8\",[[226],[130],[172,97]],true],[\"utf8\",[[240,159,152],[128]],true],[\"utf8\",[[240,159]],true],[\"utf8\",[[204],[204,140]],true],[\"utf8\",[[242,144],[208]],true],[\"utf8\",[[246,144],[208]],true],[\"utf8\",[[255,97,195]],true],[\"utf8\",[[237,160,128]],true],[\"utf8\",[[97,226,130]],false],[\"utf16le\",[[97,0,98]],true],[\"utf16le\",[[61,216],[0,222]],true],[\"utf16le\",[[61,216]],true],[\"utf16le\",[[61],[216,77],[220]],true],[\"base64\",[[1],[2,3,4]],true],[\"base64\",[[1,2]],true],[\"base64url\",[[251,255],[254]],true],[\"hex\",[[222,173],[190,239]],true],[\"latin1\",[[233,255]],true],[\"ascii\",[[233,65]],true]]";
const EXPECTED: &str = "[{\"enc\":\"utf8\",\"chunks\":[[226,130],[172]],\"end\":true,\"parts\":[\"\",\"€\"],\"tail\":\"\",\"state\":[0,0,[226,130,172,0]]},{\"enc\":\"utf8\",\"chunks\":[[226],[130],[172,97]],\"end\":true,\"parts\":[\"\",\"\",\"€a\"],\"tail\":\"\",\"state\":[0,0,[226,130,172,0]]},{\"enc\":\"utf8\",\"chunks\":[[240,159,152],[128]],\"end\":true,\"parts\":[\"\",\"😀\"],\"tail\":\"\",\"state\":[0,0,[240,159,152,128]]},{\"enc\":\"utf8\",\"chunks\":[[240,159]],\"end\":true,\"parts\":[\"\"],\"tail\":\"�\",\"state\":[0,0,[240,159,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[204],[204,140]],\"end\":true,\"parts\":[\"\",\"�̌\"],\"tail\":\"\",\"state\":[0,0,[204,0,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[242,144],[208]],\"end\":true,\"parts\":[\"\",\"�\"],\"tail\":\"�\",\"state\":[0,0,[208,144,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[246,144],[208]],\"end\":true,\"parts\":[\"\",\"��\"],\"tail\":\"�\",\"state\":[0,0,[208,144,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[255,97,195]],\"end\":true,\"parts\":[\"�a\"],\"tail\":\"�\",\"state\":[0,0,[195,0,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[237,160,128]],\"end\":true,\"parts\":[\"���\"],\"tail\":\"\",\"state\":[0,0,[0,0,0,0]]},{\"enc\":\"utf8\",\"chunks\":[[97,226,130]],\"end\":false,\"parts\":[\"a\"],\"tail\":null,\"state\":[1,3,[226,130,0,0]]},{\"enc\":\"utf16le\",\"chunks\":[[97,0,98]],\"end\":true,\"parts\":[\"a\"],\"tail\":\"\",\"state\":[0,0,[98,0,0,0]]},{\"enc\":\"utf16le\",\"chunks\":[[61,216],[0,222]],\"end\":true,\"parts\":[\"\",\"😀\"],\"tail\":\"\",\"state\":[0,0,[61,216,0,222]]},{\"enc\":\"utf16le\",\"chunks\":[[61,216]],\"end\":true,\"parts\":[\"\"],\"tail\":\"\\ud83d\",\"state\":[0,0,[61,216,0,0]]},{\"enc\":\"utf16le\",\"chunks\":[[61],[216,77],[220]],\"end\":true,\"parts\":[\"\",\"\\ud83d\",\"\\udc4d\"],\"tail\":\"\",\"state\":[0,0,[77,220,0,0]]},{\"enc\":\"base64\",\"chunks\":[[1],[2,3,4]],\"end\":true,\"parts\":[\"\",\"AQID\"],\"tail\":\"BA==\",\"state\":[0,0,[4,2,3,0]]},{\"enc\":\"base64\",\"chunks\":[[1,2]],\"end\":true,\"parts\":[\"\"],\"tail\":\"AQI=\",\"state\":[0,0,[1,2,0,0]]},{\"enc\":\"base64url\",\"chunks\":[[251,255],[254]],\"end\":true,\"parts\":[\"\",\"-__-\"],\"tail\":\"\",\"state\":[0,0,[251,255,254,0]]},{\"enc\":\"hex\",\"chunks\":[[222,173],[190,239]],\"end\":true,\"parts\":[\"dead\",\"beef\"],\"tail\":\"\",\"state\":[0,0,[0,0,0,0]]},{\"enc\":\"latin1\",\"chunks\":[[233,255]],\"end\":true,\"parts\":[\"éÿ\"],\"tail\":\"\",\"state\":[0,0,[0,0,0,0]]},{\"enc\":\"ascii\",\"chunks\":[[233,65]],\"end\":true,\"parts\":[\"iA\"],\"tail\":\"\",\"state\":[0,0,[0,0,0,0]]}]";

#[test]
fn writes_and_ends_as_javascriptcore() {
    realm(|global| {
        global
            .to_js_value()
            .put(global, "sequences", JSValue::from_utf8(SEQUENCES.as_bytes()));
        let actual = eval(
            global,
            "JSON.stringify(JSON.parse(sequences).map(([enc, chunks, end]) => { const d = new StringDecoder(enc); const parts = chunks.map(c => d.write(new Uint8Array(c))); const tail = end ? d.end() : null; return { enc, chunks, end, parts, tail, state: [d.lastNeed, d.lastTotal, Array.from(d.lastChar)] } }))",
        );
        let actual: Vec<&str> = actual.split("},{").collect();
        let expected: Vec<&str> = EXPECTED.split("},{").collect();
        let differ: Vec<String> = expected
            .iter()
            .zip(&actual)
            .filter(|(e, a)| e != a)
            .map(|(e, a)| format!("expected: {e}\n  actual: {a}"))
            .collect();
        assert_eq!(actual.len(), expected.len());
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    });
}

#[test]
fn the_class() {
    let cases: &[(&str, &str)] = &[
        ("StringDecoder.name", "string:StringDecoder"),
        ("Object.prototype.toString.call(new StringDecoder())", "string:[object StringDecoder]"),
        ("Object.keys(StringDecoder.prototype)", "[object Array]:[\"lastChar\",\"lastNeed\",\"lastTotal\",\"encoding\",\"write\",\"end\",\"text\"]"),
        ("['utf8','UTF-8','utf-8','ucs2','UCS-2','utf16le','utf-16le','latin1','binary','base64','base64url','hex','ascii','Hex'].map(e => new StringDecoder(e).encoding)", "[object Array]:[\"utf8\",\"utf8\",\"utf8\",\"utf16le\",\"utf16le\",\"utf16le\",\"utf16le\",\"latin1\",\"latin1\",\"base64\",\"base64url\",\"hex\",\"ascii\",\"hex\"]"),
        ("new StringDecoder('nope')", "throws TypeError: Unknown encoding: nope (ERR_UNKNOWN_ENCODING)"),
        ("new StringDecoder(1)", "throws TypeError: Unknown encoding: 1 (ERR_UNKNOWN_ENCODING)"),
        ("new StringDecoder(null).encoding", "string:utf8"),
        ("new StringDecoder('')", "throws TypeError: Unknown encoding:  (ERR_UNKNOWN_ENCODING)"),
        ("new StringDecoder().write('abc')", "string:abc"),
        ("new StringDecoder().write(1)", "throws TypeError: The \"buf\" argument must be of type Buffer, TypedArray, or DataView. Received type number (1) (ERR_INVALID_ARG_TYPE)"),
        ("new StringDecoder().write(null)", "throws TypeError: The \"buf\" argument must be of type Buffer, TypedArray, or DataView. Received null (ERR_INVALID_ARG_TYPE)"),
        ("new StringDecoder().write()", "throws TypeError: Not enough arguments (ERR_MISSING_ARGS)"),
        ("new StringDecoder().write({})", "throws TypeError: The \"buf\" argument must be of type Buffer, TypedArray, or DataView. Received an instance of Object (ERR_INVALID_ARG_TYPE)"),
        ("new StringDecoder().end(1)", "throws TypeError: Expected Uint8Array (undefined)"),
        ("new StringDecoder().end()", "string:"),
        ("new StringDecoder().text(new Uint8Array([97]))", "throws TypeError: Not enough arguments (ERR_MISSING_ARGS)"),
        ("new StringDecoder().text(new Uint8Array([97,98]), 1)", "string:b"),
        ("new StringDecoder().text(new Uint8Array([97,98]), 5)", "string:"),
        ("new StringDecoder().text(new Uint8Array([97,98]), -1)", "string:"),
        ("StringDecoder.prototype.write.call({}, new Uint8Array(1))", "throws TypeError: Can only call StringDecoder.write on instances of StringDecoder (ERR_INVALID_THIS)"),
        ("Object.getOwnPropertyDescriptor(StringDecoder.prototype, 'lastChar').get.call({})", "throws TypeError: The StringDecoder.lastChar getter can only be used on instances of StringDecoder (undefined)"),
        ("StringDecoder.prototype.write.call({ __intrinsic__decode: new StringDecoder('hex') }, new Uint8Array([255]))", "string:ff"),
        ("new StringDecoder('ascii').write(new Uint8Array([0x61, 0xe9, 0xff]))", "string:ai\u{7f}"),
        ("new StringDecoder().write(new DataView(new Uint8Array([0x61,0x62]).buffer, 1))", "string:b"),
        ("new StringDecoder().write(new Uint16Array([0x6261]))", "string:ab"),
        ("new StringDecoder().write(new Uint8Array(new SharedArrayBuffer(1)))", "string:\u{0}"),
        ("Object.getOwnPropertyDescriptor(StringDecoder.prototype, 'lastNeed').get.name", "string:get lastNeed"),
        ("[StringDecoder.prototype.write.length, StringDecoder.prototype.end.length, StringDecoder.prototype.text.length]", "[object Array]:[1,1,2]"),
    ];
    realm(|global| {
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|&(source, expected)| {
                let actual = eval(global, &format!("r(() => ({source}))"));
                (actual != expected).then(|| format!("{source}\n    expected: {expected}\n    actual:   {actual}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    });
}
