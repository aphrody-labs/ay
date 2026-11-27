//! `Bun::createNodeVMBinding` (src/jsc/bindings/NodeVM.cpp), what src/js/node/vm.ts takes, on bun_js. A context is a
//! Boa realm of its own, which bun_js holds in a handle; vm.js (beside this file) builds `Script`, `createContext`
//! and the rest on the natives here. As JavaScriptCore's `NodeVMGlobalObject`, the realm's global forwards its
//! properties to the contextified object (Boa's `Context::create_contextified_realm`).

use bun_js::{CallFrame, JSFunction, JSGlobalObject, JSObject, JSValue, JsResult};

/// The parts of vm.ts's binding that are JavaScript: `Script`, contexts, `compileFunction`.
const BINDING: &str = include_str!("vm.js");

fn string(global: &JSGlobalObject, value: &JSValue, otherwise: &str) -> JsResult<String> {
    if value.is_undefined() { Ok(otherwise.to_owned()) } else { value.to_utf8(global) }
}

/// `createRealm(object?)`: a handle of a new realm, whose global forwards to `object` when there is one.
fn create_realm(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let target = frame.argument(0);
    if target.is_undefined() { global.create_realm_handle() } else { global.create_contextified_realm_handle(&target) }
}

/// `realmGlobal(handle)`: its global object.
fn realm_global(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    global.realm_global_object(&frame.argument(0))
}

/// `evaluate(handle | undefined, code, filename)`: runs `code` as a script of that realm, or of this one.
fn evaluate(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let handle = frame.argument(0);
    let code = frame.argument(1).to_utf8(global)?;
    let filename = string(global, &frame.argument(2), "evalmachine.<anonymous>")?;
    global.evaluate_script_in((!handle.is_undefined()).then_some(&handle), &code, &filename)
}

/// `check(code, filename)`: the SyntaxError of `code`, thrown, as `new vm.Script()` throws it.
fn check(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let code = frame.argument(0).to_utf8(global)?;
    let filename = string(global, &frame.argument(1), "evalmachine.<anonymous>")?;
    global.check_script(&code, &filename)?;
    Ok(JSValue::UNDEFINED)
}

fn is_module_namespace_object(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    Ok(JSValue::js_boolean(global.is_module_namespace(&frame.argument(0))))
}

/// What `$cpp("NodeVM.cpp", "Bun::createNodeVMBinding")` answers.
pub fn create_binding(global: &JSGlobalObject) -> JsResult<JSValue> {
    let natives = JSObject::create(global);
    for (name, f, length) in [
        ("createRealm", create_realm as bun_js::JSHostFn, 0),
        ("realmGlobal", realm_global, 1),
        ("evaluate", evaluate, 3),
        ("check", check, 2),
        ("isModuleNamespaceObject", is_module_namespace_object, 1),
    ] {
        natives.put(global, name, JSFunction::create(global, name, f, length));
    }
    let make = global.evaluate_script(BINDING)?;
    make.call(global, JSValue::UNDEFINED, &[natives.to_js()])
}

pub fn native(global: &JSGlobalObject, symbol: &str) -> Option<JsResult<JSValue>> {
    match symbol {
        "Bun::createNodeVMBinding" => Some(create_binding(global)),
        _ => None,
    }
}
