//! ES module loading: Bun-style resolution (extension and `index` probing, `node_modules`, `package.json`),
//! every module transpiled by Bun's transpiler before Boa parses it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module,
    NativeFunction, Source, js_string,
};

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "json"];

#[derive(Default)]
pub struct BunModuleLoader {
    modules: RefCell<HashMap<PathBuf, Module>>,
    /// The `module` object of each CommonJS module, whose `exports` `require` answers.
    commonjs: RefCell<HashMap<PathBuf, JsObject>>,
    /// What `mock.module()` put in place of a path or a specifier: the exports that `require` answers.
    mocked: RefCell<HashMap<PathBuf, JsValue>>,
}

/// The loader of the realm, in its data, which `require` (a native function without captures of Rust values) loads
/// through.
struct Installed(Rc<BunModuleLoader>);

/// The loader that `install` made the realm's.
pub fn installed(context: &Context) -> Option<Rc<BunModuleLoader>> {
    context
        .get_data::<Installed>()
        .map(|installed| Rc::clone(&installed.0))
}

/// `require` as a module in `dir` calls it: Bun's builtin modules, JSON, CommonJS modules (their
/// `module.exports`) and ES modules (their namespace, once evaluated).
pub fn make_require(dir: &Path, context: &mut Context) -> boa_engine::object::builtins::JsFunction {
    let dir = JsString::from(dir.display().to_string().as_str());
    let function = NativeFunction::from_copy_closure_with_captures(
        |_, args, dir: &JsString, context| {
            let specifier = args
                .get_or_undefined(0)
                .to_string(context)?
                .to_std_string_escaped();
            let dir = PathBuf::from(dir.to_std_string_escaped());
            require(&specifier, &dir, context)
        },
        dir,
    );
    boa_engine::object::FunctionObjectBuilder::new(context.realm(), function)
        .name(js_string!("require"))
        .length(1)
        .build()
}

fn require(specifier: &str, dir: &Path, context: &mut Context) -> JsResult<JsValue> {
    let loader = context
        .get_data::<Installed>()
        .map(|installed| Rc::clone(&installed.0))
        .ok_or_else(|| JsNativeError::error().with_message("no module loader"))?;
    let resolved = resolve(specifier, dir);
    let key = resolved.clone().unwrap_or_else(|| PathBuf::from(specifier));
    if let Some(exports) = loader.mocked.borrow().get(&key) {
        return Ok(exports.clone());
    }
    let module = if let Some(module) = loader.builtin(specifier, context) {
        if let Some(id) =
            crate::builtins::id_of(specifier).filter(|_| !matches!(specifier, "bun" | "bun:test"))
        {
            return crate::builtins::require(id, context);
        }
        module?
    } else {
        let Some(path) = resolved else {
            return Err(type_error(format!(
                "Cannot find module '{specifier}' from '{}'",
                dir.display()
            )));
        };
        let module = loader.load(&path, context)?;
        if let Some(object) = loader.commonjs.borrow().get(&path) {
            return object.get(js_string!("exports"), context);
        }
        module
    };
    let promise = module.load_link_evaluate(context);
    context.run_jobs()?;
    if let boa_engine::builtins::promise::PromiseState::Rejected(reason) = promise.state() {
        return Err(JsError::from_opaque(reason));
    }
    Ok(module.namespace(context).into())
}

fn type_error(message: String) -> JsError {
    JsNativeError::typ().with_message(message).into()
}

fn file(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

/// What `./x.js` may name besides itself, as `rewritten_file_extensions` of src/resolver/resolver.rs: TypeScript's
/// `./x.ts` for `./x.js` (esbuild's `rewrittenFileExtensions`, and Bun's `.mts`).
fn rewritten_file_extensions(path: &Path) -> &'static [&'static str] {
    let inside_node_modules = || path.components().any(|part| part.as_os_str() == "node_modules");
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("js" | "jsx") => &["ts", "tsx", "mts"],
        Some("mjs" | "cjs")
            if bun_core::feature_flags::DISABLE_AUTO_JS_TO_TS_IN_NODE_MODULES && inside_node_modules() =>
        {
            &[]
        }
        Some("mjs") => &["mts"],
        Some("cjs") => &["cts"],
        _ => &[],
    }
}

fn with_extension(path: &Path) -> Option<PathBuf> {
    file(path)
        .or_else(|| rewritten_file_extensions(path).iter().find_map(|ext| file(&path.with_extension(ext))))
        .or_else(|| {
            EXTENSIONS.iter().find_map(|ext| {
                let mut candidate = path.as_os_str().to_owned();
                candidate.push(".");
                candidate.push(ext);
                file(Path::new(&candidate))
            })
        })
}

fn package_entry(dir: &Path) -> Option<PathBuf> {
    let json = bun_sys::File::read_from(
        bun_sys::Fd::cwd(),
        dir.join("package.json").as_os_str().as_encoded_bytes(),
    )
    .ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let exports = manifest.get("exports").and_then(|e| match e {
        serde_json::Value::String(s) => Some(s.as_str()),
        serde_json::Value::Object(map) => {
            let root = map.get(".").unwrap_or(e);
            root.as_str().or_else(|| {
                ["bun", "import", "default"]
                    .iter()
                    .find_map(|k| root.get(k).and_then(serde_json::Value::as_str))
            })
        }
        _ => None,
    });
    let entry = exports
        .or_else(|| manifest.get("module").and_then(serde_json::Value::as_str))
        .or_else(|| manifest.get("main").and_then(serde_json::Value::as_str))?;
    with_extension(&dir.join(entry))
}

fn as_file_or_dir(path: &Path) -> Option<PathBuf> {
    with_extension(path)
        .or_else(|| path.is_dir().then(|| package_entry(path)).flatten())
        .or_else(|| {
            EXTENSIONS
                .iter()
                .find_map(|ext| file(&path.join(format!("index.{ext}"))))
        })
}

pub fn resolve(specifier: &str, from_dir: &Path) -> Option<PathBuf> {
    let specifier = specifier.strip_prefix("file://").unwrap_or(specifier);
    if specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier == "."
        || specifier == ".."
    {
        return as_file_or_dir(&from_dir.join(specifier)).and_then(|p| p.canonicalize().ok());
    }
    tsconfig_paths(specifier, from_dir)
        .or_else(|| {
            from_dir
                .ancestors()
                .find_map(|dir| as_file_or_dir(&dir.join("node_modules").join(specifier)))
        })
        .and_then(|p| p.canonicalize().ok())
}

/// JSONC (comments, trailing commas) as JSON.
fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                out.push_str(&text[start..i]);
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                let rest = text[i + 1..].trim_start();
                if !(rest.starts_with('}') || rest.starts_with(']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                let start = i;
                i += 1;
                while i < bytes.len() && !matches!(bytes[i], b'"' | b'/' | b',') {
                    i += 1;
                }
                out.push_str(&text[start..i]);
            }
        }
    }
    out
}

/// `compilerOptions.paths` of the nearest tsconfig.json that has them, as Bun's resolver reads them:
/// `"harness": ["./harness.ts"]`, `"_util/*": ["./_util/*"]`, relative to `baseUrl` or the tsconfig.json.
fn tsconfig_paths(specifier: &str, from_dir: &Path) -> Option<PathBuf> {
    for dir in from_dir.ancestors() {
        let Ok(text) = bun_sys::File::read_from(
            bun_sys::Fd::cwd(),
            dir.join("tsconfig.json").as_os_str().as_encoded_bytes(),
        ) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(
            &String::from_utf8_lossy(&text),
        )) else {
            continue;
        };
        let options = &config["compilerOptions"];
        let Some(paths) = options["paths"].as_object() else {
            continue;
        };
        let base = options["baseUrl"]
            .as_str()
            .map_or(dir.to_path_buf(), |b| dir.join(b));
        // The longest prefix wins, as in TypeScript.
        let mut best: Option<(usize, &serde_json::Value, String)> = None;
        for (pattern, targets) in paths {
            let star = match pattern.split_once('*') {
                None if pattern == specifier => String::new(),
                Some((prefix, suffix))
                    if specifier.starts_with(prefix)
                        && specifier.ends_with(suffix)
                        && specifier.len() >= prefix.len() + suffix.len() =>
                {
                    specifier[prefix.len()..specifier.len() - suffix.len()].to_owned()
                }
                _ => continue,
            };
            let rank = pattern.find('*').unwrap_or(usize::MAX);
            if best.as_ref().is_none_or(|(r, ..)| rank > *r) {
                best = Some((rank, targets, star));
            }
        }
        let (_, targets, star) = best?;
        return targets
            .as_array()?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find_map(|target| as_file_or_dir(&base.join(target.replace('*', &star))));
    }
    None
}

impl BunModuleLoader {
    /// The modules built into the binary: `bun:test` (and the modules of js/test it imports) and `bun`.
    pub fn builtin(&self, name: &str, context: &mut Context) -> Option<JsResult<Module>> {
        let source: &[u8] = match name {
            "bun:test" => include_bytes!("js/test.ts"),
            "bun:test/equals.ts" => include_bytes!("js/test/equals.ts"),
            "bun:test/expect.ts" => include_bytes!("js/test/expect.ts"),
            "bun:test/format.ts" => include_bytes!("js/test/format.ts"),
            "bun:test/mock.ts" => include_bytes!("js/test/mock.ts"),
            "bun:test/runner.ts" => include_bytes!("js/test/runner.ts"),
            "bun:test/snapshot.ts" => include_bytes!("js/test/snapshot.ts"),
            "bun:test/source.ts" => include_bytes!("js/test/source.ts"),
            "bun:test/state.ts" => include_bytes!("js/test/state.ts"),
            "bun" => include_bytes!("js/bun_module.ts"),
            // The helpers the transpiler imports (`__using`, `__callDispose`, …): src/runtime.bun.js, whose
            // "./runtime" is src/runtime.js.
            "bun:wrap" => include_bytes!("../../../src/runtime.bun.js"),
            "bun:wrap/runtime" => include_bytes!("../../../src/runtime.js"),
            _ => {
                let id = crate::builtins::id_of(name)?;
                return Some(self.builtin_module(name, id, context));
            }
        };
        let key = PathBuf::from(name);
        if let Some(module) = self.modules.borrow().get(&key) {
            return Some(Ok(module.clone()));
        }
        let module = crate::transpile::transpile(format!("{name}.ts").as_bytes(), source)
            .map_err(|err| JsNativeError::syntax().with_message(err).into())
            .and_then(|js| Module::parse(Source::from_bytes(&js).with_path(&key), None, context));
        if let Ok(module) = &module {
            self.modules.borrow_mut().insert(key, module.clone());
        }
        Some(module)
    }

    /// The ES module of Bun's builtin module `id` (src/js), made once.
    fn builtin_module(&self, name: &str, id: usize, context: &mut Context) -> JsResult<Module> {
        let key = PathBuf::from(name);
        if let Some(module) = self.modules.borrow().get(&key) {
            return Ok(module.clone());
        }
        let module = crate::builtins::module(id, context)?;
        self.modules.borrow_mut().insert(key, module.clone());
        Ok(module)
    }

    /// Makes this the loader `require` loads through in `context`.
    pub fn install(self: &Rc<Self>, context: &mut Context) {
        context.insert_data(Installed(Rc::clone(self)));
    }

    /// Runs a CommonJS module, transpiled to a function of `(exports, require, module, __filename, __dirname,
    /// importMeta)`, and answers an ES module whose `default` is its `module.exports` and whose names are their keys.
    fn commonjs_module(&self, path: &Path, code: &[u8], context: &mut Context) -> JsResult<Module> {
        let function = context.eval(Source::from_bytes(code).with_path(path))?;
        let function = function
            .as_callable()
            .ok_or_else(|| {
                type_error(format!(
                    "{}: a CommonJS module is a function",
                    path.display()
                ))
            })?
            .clone();
        let dir = path.parent().unwrap_or(path);
        let filename = JsString::from(path.display().to_string().as_str());
        let require = make_require(dir, context);
        let exports = JsObject::with_object_proto(context.intrinsics());
        let module = JsObject::with_object_proto(context.intrinsics());
        module.set(js_string!("exports"), exports.clone(), false, context)?;
        module.set(js_string!("id"), filename.clone(), false, context)?;
        module.set(js_string!("filename"), filename.clone(), false, context)?;
        module.set(js_string!("loaded"), false, false, context)?;
        module.set(js_string!("require"), require.clone(), false, context)?;
        self.commonjs
            .borrow_mut()
            .insert(path.to_path_buf(), module.clone());
        let import_meta = JsObject::with_object_proto(context.intrinsics());
        fill_import_meta(&import_meta, path, context);
        let args = [
            exports.clone().into(),
            require.into(),
            module.clone().into(),
            filename.into(),
            JsString::from(dir.display().to_string().as_str()).into(),
            import_meta.into(),
        ];
        function.call(&exports.into(), &args, context)?;
        module.set(js_string!("loaded"), true, false, context)?;
        let exports = module.get(js_string!("exports"), context)?;
        exports_module(exports, path, false, context)
    }

    /// `mock.module(specifier, factory)`: from now on, what imports or requires `key` (a resolved path, or the
    /// specifier when it resolves to nothing) gets `exports`.
    pub fn mock(&self, key: PathBuf, exports: JsValue, context: &mut Context) -> JsResult<()> {
        let module = exports_module(exports.clone(), &key, true, context)?;
        self.modules.borrow_mut().insert(key.clone(), module);
        self.mocked.borrow_mut().insert(key, exports);
        Ok(())
    }

    /// The module that `mock` put in place of `key`.
    fn mocked_module(&self, key: &Path) -> Option<Module> {
        if !self.mocked.borrow().contains_key(key) {
            return None;
        }
        self.modules.borrow().get(key).cloned()
    }
}

/// An ES module whose names are the keys of `exports`: what a CommonJS module and a mocked one are seen as from an
/// `import`. Its `default` is `exports` itself, or, for a mocked module, its `default` where it has one.
fn exports_module(exports: JsValue, path: &Path, own_default: bool, context: &mut Context) -> JsResult<Module> {
    {
        let mut names = vec![js_string!("default")];
        let default = match exports.as_object() {
            Some(object) if own_default && object.has_own_property(js_string!("default"), context)? => {
                object.get(js_string!("default"), context)?
            }
            _ => exports.clone(),
        };
        let mut values = vec![default];
        if let Some(object) = exports.as_object() {
            let keys = boa_engine::object::builtins::JsArray::from_object(
                context
                    .intrinsics()
                    .constructors()
                    .object()
                    .constructor()
                    .get(js_string!("keys"), context)?
                    .as_callable()
                    .expect("Object.keys")
                    .call(&JsValue::undefined(), &[object.clone().into()], context)?
                    .as_object()
                    .expect("an array")
                    .clone(),
            )?;
            for i in 0..keys.length(context)? {
                let name = keys.get(i, context)?.to_string(context)?;
                if name == js_string!("default") {
                    continue;
                }
                values.push(object.get(name.clone(), context)?);
                names.push(name);
            }
        }
        let pairs: Vec<(JsString, JsValue)> = names.iter().cloned().zip(values).collect();
        let initializer =
            boa_engine::module::SyntheticModuleInitializer::from_copy_closure_with_captures(
                |module, pairs: &Vec<(JsString, JsValue)>, _| {
                    for (name, value) in pairs {
                        module.set_export(name, value.clone())?;
                    }
                    Ok(())
                },
                pairs,
            );
        Ok(Module::synthetic(
            &names,
            initializer,
            Some(path.to_path_buf()),
            None,
            context,
        ))
    }
}

impl BunModuleLoader {
    pub fn load(&self, path: &Path, context: &mut Context) -> JsResult<Module> {
        if let Some(module) = self.modules.borrow().get(path) {
            return Ok(module.clone());
        }
        let contents =
            bun_sys::File::read_from(bun_sys::Fd::cwd(), path.as_os_str().as_encoded_bytes())
                .map_err(|err| type_error(err.to_string()))?;
        self.load_source(path, &contents, context)
    }

    /// The module of `contents`, as if read from `path`: a worker's `data:` or `blob:` entry point.
    pub fn load_source(&self, path: &Path, contents: &[u8], context: &mut Context) -> JsResult<Module> {
        let module = if path.extension().is_some_and(|ext| ext == "json") {
            let text = String::from_utf8_lossy(contents);
            Module::parse_json(JsString::from(text.as_ref()), context)?
        } else {
            let js =
                crate::transpile::transpile_module(path.as_os_str().as_encoded_bytes(), contents)
                    .map_err(|err| JsNativeError::syntax().with_message(err))?;
            if js.commonjs {
                let module = self.commonjs_module(path, &js.code, context)?;
                self.modules
                    .borrow_mut()
                    .insert(path.to_path_buf(), module.clone());
                return Ok(module);
            }
            Module::parse(Source::from_bytes(&js.code).with_path(path), None, context)?
        };
        self.modules
            .borrow_mut()
            .insert(path.to_path_buf(), module.clone());
        Ok(module)
    }
}

impl ModuleLoader for BunModuleLoader {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl Future<Output = JsResult<Module>> {
        let mut specifier = request.specifier().to_std_string_escaped();
        if specifier == "./runtime" && referrer.path().is_some_and(|p| p == Path::new("bun:wrap")) {
            specifier = "bun:wrap/runtime".to_owned();
        }
        // bun:test's modules import each other relatively: "bun:test" is the file "test" in "bun:", and
        // "./test/expect.ts" from it is "bun:test/expect.ts", whose "./state.ts" is "bun:test/state.ts".
        if let (Some(relative), Some(from)) = (
            specifier.strip_prefix("./"),
            referrer.path().and_then(Path::to_str).filter(|p| p.starts_with("bun:test")),
        ) {
            let dir = &from[..from.rfind(['/', ':']).map_or(0, |i| i + 1)];
            specifier = format!("{dir}{relative}");
        }
        if let Some(module) = self.mocked_module(Path::new(&specifier)) {
            return std::future::ready(Ok(module));
        }
        if let Some(module) = self.builtin(&specifier, &mut context.borrow_mut()) {
            return std::future::ready(module);
        }
        let from_dir = referrer
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let result = match resolve(&specifier, &from_dir) {
            // A path that `mock.module()` mocked is in `modules`, where `load` finds it first.
            Some(path) => self.load(&path, &mut context.borrow_mut()),
            None => Err(type_error(format!(
                "Cannot find module '{specifier}' from '{}'",
                from_dir.display()
            ))),
        };
        std::future::ready(result)
    }

    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        let Some(path) = module.path() else { return };
        fill_import_meta(import_meta, path, context);
    }
}

/// `import.meta` of the module at `path`, an ES module's or a CommonJS module's (its wrapper's `importMeta`).
fn fill_import_meta(import_meta: &JsObject, path: &Path, context: &mut Context) {
    let dir = path.parent().unwrap_or(path);
    let name = path.file_name().unwrap_or_default();
    let set = |key: JsString, value: String, context: &mut Context| {
        let _ = import_meta.set(key, JsString::from(value.as_str()), false, context);
    };
    set(js_string!("url"), format!("file://{}", path.display()), context);
    set(js_string!("path"), path.display().to_string(), context);
    set(js_string!("filename"), path.display().to_string(), context);
    set(js_string!("file"), name.to_string_lossy().into_owned(), context);
    set(js_string!("dir"), dir.display().to_string(), context);
    set(js_string!("dirname"), dir.display().to_string(), context);
    let main = crate::runtime::is_main(path);
    let _ = import_meta.set(js_string!("main"), main, false, context);
    let require = make_require(dir, context);
    let _ = import_meta.set(js_string!("require"), require, false, context);
}
