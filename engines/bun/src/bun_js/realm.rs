//! Realms besides the global object's own: what node:vm's contexts are (JavaScriptCore's `NodeVMGlobalObject`). A
//! realm has its own global object and intrinsics; values pass between realms as they are. A contextified realm's
//! global forwards its properties to the object it was made for.

use boa_engine::object::JsData;
use boa_engine::realm::Realm;
use boa_engine::{JsObject, Script, Source};
use boa_gc::{Finalize, Trace};

use crate::{JSGlobalObject, JSValue, JsResult};

/// What a realm handle object holds.
#[derive(Trace, Finalize)]
struct RealmHandle(Realm);

impl JsData for RealmHandle {}

impl JSGlobalObject {
    /// A new realm with the default global bindings, as an object that holds it.
    pub fn create_realm_handle(&self) -> JsResult<JSValue> {
        let realm = self.cx().create_realm();
        let realm = self.lift(realm)?;
        Ok(JSValue(JsObject::from_proto_and_data(None, RealmHandle(realm)).into()))
    }

    /// A new realm whose global forwards to `target` (Boa's `create_contextified_realm`), as an object that holds it.
    pub fn create_contextified_realm_handle(&self, target: &JSValue) -> JsResult<JSValue> {
        let Some(target) = target.0.as_object() else {
            return Err(self.throw_type_error(format_args!("a context must be an object")));
        };
        let realm = self.cx().create_contextified_realm(target);
        let realm = self.lift(realm)?;
        Ok(JSValue(JsObject::from_proto_and_data(None, RealmHandle(realm)).into()))
    }

    fn realm_of(&self, handle: &JSValue) -> JsResult<Realm> {
        match handle.0.as_object().and_then(|object| object.downcast_ref::<RealmHandle>().map(|h| h.0.clone())) {
            Some(realm) => Ok(realm),
            None => Err(self.throw_type_error(format_args!("not a realm"))),
        }
    }

    /// The global object of the realm that `handle` holds.
    pub fn realm_global_object(&self, handle: &JSValue) -> JsResult<JSValue> {
        let realm = self.realm_of(handle)?;
        let outer = self.cx().enter_realm(realm);
        let global = self.cx().global_object();
        self.cx().enter_realm(outer);
        Ok(JSValue(global.into()))
    }

    /// Runs `source` as a script of the realm `handle` holds, or of this global object's for `None`; a syntax error
    /// throws before anything runs.
    pub fn evaluate_script_in(&self, handle: Option<&JSValue>, source: &str, filename: &str) -> JsResult<JSValue> {
        let realm = match handle {
            Some(handle) => Some(self.realm_of(handle)?),
            None => None,
        };
        let outer = realm.map(|realm| self.cx().enter_realm(realm));
        let script = Script::parse(Source::from_bytes(source).with_path(std::path::Path::new(filename)), None, self.cx());
        let result = script.and_then(|script| script.evaluate(self.cx()));
        if let Some(outer) = outer {
            self.cx().enter_realm(outer);
        }
        self.lift(result).map(JSValue)
    }

    /// Parses `source` as a script, which throws its `SyntaxError`, without running it.
    pub fn check_script(&self, source: &str, filename: &str) -> JsResult<()> {
        let script = Script::parse(Source::from_bytes(source).with_path(std::path::Path::new(filename)), None, self.cx());
        self.lift(script).map(drop)
    }

    /// A module namespace object (`import * as ns`).
    pub fn is_module_namespace(&self, value: &JSValue) -> bool {
        value.0.as_object().is_some_and(|object| object.is::<boa_engine::module::ModuleNamespace>())
    }
}
