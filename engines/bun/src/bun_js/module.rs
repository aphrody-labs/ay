//! ES modules: modules of source text, modules whose exports Rust makes (`bun:*`, `node:*`), and the loader that
//! finds the latter before it asks the embedder's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer, SyntheticModuleInitializer};
use boa_engine::{Context, JsString, JsValue, Module, Source};

use crate::{JSGlobalObject, JSValue, JsResult};

/// A module.
#[derive(Clone, Debug)]
pub struct JSModule(Module);

/// The exports of a native module, made the first time a module imports it.
pub type NativeModuleFn = fn(&JSGlobalObject) -> JsResult<Vec<(&'static str, JSValue)>>;

#[derive(Default)]
pub(crate) struct NativeModules {
    factories: HashMap<String, NativeModuleFn>,
    loaded: HashMap<String, Module>,
}

impl JSModule {
    pub fn from_engine(module: Module) -> JSModule {
        JSModule(module)
    }

    pub fn as_engine(&self) -> &Module {
        &self.0
    }

    /// A module whose exports are these values.
    pub fn synthetic(global: &JSGlobalObject, exports: Vec<(&str, JSValue)>) -> JSModule {
        let exports: Vec<(JsString, JsValue)> = exports
            .into_iter()
            .map(|(name, value)| (JsString::from(name), value.0))
            .collect();
        let names: Vec<JsString> = exports.iter().map(|(name, _)| name.clone()).collect();
        let initializer = SyntheticModuleInitializer::from_copy_closure_with_captures(
            |module, exports: &Vec<(JsString, JsValue)>, _| {
                for (name, value) in exports {
                    module.set_export(name, value.clone())?;
                }
                Ok(())
            },
            exports,
        );
        JSModule(Module::synthetic(
            &names,
            initializer,
            None,
            None,
            global.cx(),
        ))
    }

    /// Parses source text as a module.
    pub fn parse(global: &JSGlobalObject, source: &str, path: Option<&Path>) -> JsResult<JSModule> {
        let source = Source::from_bytes(source);
        let source = match path {
            Some(path) => source.with_path(path),
            None => source,
        };
        let result = Module::parse(source, None, global.cx());
        global.lift(result).map(JSModule)
    }

    /// Loads, links and evaluates the module and what it imports, runs the jobs that queues, and answers its
    /// namespace. A module that awaits something no job settles is an error.
    pub fn evaluate(&self, global: &JSGlobalObject) -> JsResult<JSValue> {
        let promise = self.0.load_link_evaluate(global.cx());
        global.drain_microtasks()?;
        match promise.state() {
            PromiseState::Fulfilled(_) => Ok(self.namespace(global)),
            PromiseState::Rejected(reason) => Err(global.throw_value(JSValue(reason))),
            PromiseState::Pending => {
                Err(global.throw(format_args!("the module did not finish evaluating")))
            }
        }
    }

    /// The module namespace object.
    pub fn namespace(&self, global: &JSGlobalObject) -> JSValue {
        JSValue(self.0.namespace(global.cx()).into())
    }
}

impl JSGlobalObject {
    /// What `import "specifier"` finds in this realm: the exports `exports` makes.
    pub fn register_native_module(&self, specifier: &str, exports: NativeModuleFn) {
        let modules = &mut self.state().modules;
        modules.loaded.remove(specifier);
        modules.factories.insert(specifier.to_owned(), exports);
    }

    /// The native module registered as `specifier`, made the first time.
    pub fn native_module(&self, specifier: &str) -> JsResult<Option<JSModule>> {
        if let Some(module) = self.state().modules.loaded.get(specifier) {
            return Ok(Some(JSModule(module.clone())));
        }
        let Some(&exports) = self.state().modules.factories.get(specifier) else {
            return Ok(None);
        };
        let module = JSModule::synthetic(self, exports(self)?);
        self.state()
            .modules
            .loaded
            .insert(specifier.to_owned(), module.0.clone());
        Ok(Some(module))
    }
}

/// A module loader that answers the native modules of the realm, and asks `inner` for every other.
pub struct Loader<L> {
    inner: Rc<L>,
}

impl<L> Loader<L> {
    pub fn new(inner: Rc<L>) -> Loader<L> {
        Loader { inner }
    }
}

impl<L: ModuleLoader + 'static> ModuleLoader for Loader<L> {
    async fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> boa_engine::JsResult<Module> {
        let specifier = request.specifier().to_std_string_lossy();
        let native = {
            let mut context = context.borrow_mut();
            let global = JSGlobalObject::from_context(&mut context);
            global
                .native_module(&specifier)
                .map_err(|error| global.into_engine_error(error))?
        };
        match native {
            Some(module) => Ok(module.0),
            None => {
                Rc::clone(&self.inner)
                    .load_imported_module(referrer, request, context)
                    .await
            }
        }
    }

    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &boa_engine::JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        Rc::clone(&self.inner).init_import_meta(import_meta, module, context);
    }
}
