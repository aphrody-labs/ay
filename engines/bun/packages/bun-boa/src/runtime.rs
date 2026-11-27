//! The realm a Bun program runs in: Boa's WinterTC runtime (console, timers, encoding, URL, structuredClone),
//! native operations in Rust, and `Bun`/`process` built on them by `js/bun.ts`.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
};

use crate::loader::BunModuleLoader;

thread_local! {
    /// The entry point of the realm of this thread (the process's, or a worker's): `import.meta.main` is it.
    static MAIN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Whether `path` is the entry point of this thread's realm.
pub fn is_main(path: &Path) -> bool {
    MAIN.with(|main| main.borrow().as_deref() == Some(path))
}

/// What a realm is made for: the process's program, or a worker's.
pub struct Realm<'a> {
    /// The entry point, if it is a file.
    pub main: Option<PathBuf>,
    /// `process.argv`.
    pub argv: Vec<String>,
    /// `process.env`; the process's environment when `None`.
    pub env: Option<Vec<(String, String)>>,
    pub worker: Option<&'a mut bun_runtime_js::web::worker::WorkerStart>,
}

fn string(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<String> {
    Ok(args
        .get_or_undefined(i)
        .to_string(context)?
        .to_std_string_escaped())
}

fn print(args: &[JsValue], context: &mut Context, out: &mut dyn Write) -> JsResult<JsValue> {
    let text = string(args, 0, context)?;
    let _ = out.write_all(text.as_bytes());
    Ok(JsValue::undefined())
}

fn stdout(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    print(args, context, &mut std::io::stdout().lock())
}

fn stderr(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    print(args, context, &mut std::io::stderr().lock())
}

/// `process.exit(code)`: the process, or in a worker the worker's thread.
fn exit(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = args.get_or_undefined(0).to_i32(context)?;
    if let Some(stop) = bun_runtime_js::web::worker::exit(context, code) {
        return Err(stop);
    }
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

fn cwd(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let dir = std::env::current_dir().unwrap_or_default();
    Ok(JsString::from(dir.display().to_string().as_str()).into())
}

fn nanoseconds(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    Ok((START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as f64)
        .into())
}

/// `Bun.gc()`: Boa's collection, which is always a full one.
fn gc(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    boa_gc::force_collect();
    Ok(JsValue::undefined())
}

fn strings(values: impl IntoIterator<Item = String>, context: &mut Context) -> JsValue {
    let values: Vec<JsValue> = values
        .into_iter()
        .map(|s| JsString::from(s.as_str()).into())
        .collect();
    boa_engine::object::builtins::JsArray::from_iter(values, context).into()
}

fn native(context: &mut Context, realm: &Realm<'_>) -> JsResult<JsValue> {
    let env = ObjectInitializer::new(context).build();
    let vars: Vec<(String, String)> = match &realm.env {
        Some(env) => env.clone(),
        None => std::env::vars_os()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            .collect(),
    };
    for (key, value) in vars {
        env.set(
            JsString::from(key.as_str()),
            JsString::from(value.as_str()),
            false,
            context,
        )?;
    }
    let argv = strings(realm.argv.iter().cloned(), context);
    let main = JsString::from(
        realm
            .main
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
            .as_str(),
    );
    let is_main_thread = realm.worker.is_none();
    let version = JsString::from(bun_core::package_json_version);
    let revision = JsString::from(bun_core::env::GIT_SHA);
    let serve = bun_runtime_js::server::serve_function(context)?;
    let crypto = bun_runtime_js::crypto::natives(context);
    let mut ops = ObjectInitializer::new(context);
    for (name, f, len) in [
        (
            "printStdout",
            stdout as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
            1,
        ),
        ("printStderr", stderr, 1),
        ("exit", exit, 1),
        ("cwd", cwd, 0),
        ("nanoseconds", nanoseconds, 0),
        ("gc", gc, 0),
    ] {
        ops.function(NativeFunction::from_fn_ptr(f), JsString::from(name), len);
    }
    // The crates on bun_js: the namespaces of each `.bind.ts` (`Bun.semver`), and Blob, File, Bun.file, Bun.write
    // and the standard files.
    let global = bun_js::JSGlobalObject::from_context(ops.context());
    let mut bound = bun_semver_js::generated_bindgen::semver_object::namespaces(global)
        .map_err(|error| global.into_engine_error(error))?;
    let clock = bun_runtime_js::generated_bindgen::performance::namespaces(global)
        .map_err(|error| global.into_engine_error(error))?;
    bound.extend(
        bun_runtime_js::webcore::blob::exports(global)
            .map_err(|error| global.into_engine_error(error))?,
    );
    // And what else of `Bun` bun_runtime_js implements (`Bun.Glob`, `Bun.deepEquals`).
    bound.extend(
        bun_runtime_js::bun_object(global).map_err(|error| global.into_engine_error(error))?,
    );
    let process = bun_js::JSObject::create(global);
    for (name, value) in bun_runtime_js::process_object(global) {
        process.put(global, name, value);
    }
    for (name, value) in bound.into_iter().chain(clock) {
        ops.property(JsString::from(name), value.into_engine(), Attribute::all());
    }
    ops.property(js_string!("process"), process.to_js().into_engine(), Attribute::all());
    ops.property(js_string!("serve"), serve, Attribute::all())
        .property(js_string!("crypto"), crypto, Attribute::all())
        .property(js_string!("env"), env, Attribute::all())
        .property(js_string!("argv"), argv, Attribute::all())
        .property(js_string!("main"), main, Attribute::all())
        .property(js_string!("version"), version, Attribute::all())
        .property(js_string!("revision"), revision, Attribute::all())
        .property(
            js_string!("platform"),
            JsString::from(std::env::consts::OS),
            Attribute::all(),
        )
        .property(js_string!("arch"), JsString::from(arch()), Attribute::all())
        .property(js_string!("pid"), std::process::id(), Attribute::all())
        .property(js_string!("isMainThread"), is_main_thread, Attribute::all());
    Ok(ops.build().into())
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// A realm with Bun's globals; `main` is the entry point and `argv` is `process.argv`.
pub fn context(
    loader: Rc<BunModuleLoader>,
    main: Option<PathBuf>,
    argv: Vec<String>,
) -> JsResult<Context> {
    context_for(loader, Realm { main, argv, env: None, worker: None })
}

/// A realm with Bun's globals for `realm`: the process's program, or a worker's (whose thread this is).
pub fn context_for(loader: Rc<BunModuleLoader>, mut realm: Realm<'_>) -> JsResult<Context> {
    MAIN.with(|main| *main.borrow_mut() = realm.main.clone());
    // What node:path (bun_node_path) completes a relative path with, as Bun's VM sets it at startup.
    static CWD: OnceLock<()> = OnceLock::new();
    CWD.get_or_init(|| {
        if let Ok(cwd) = std::env::current_dir() {
            bun_paths::fs::FileSystem::init(cwd.as_os_str().as_encoded_bytes());
        }
    });
    let event_loop = bun_runtime_js::EventLoop::new()
        .map_err(|err| JsNativeError::error().with_message(err.to_string()))?;
    let mut context = Context::builder()
        .module_loader(loader.clone())
        .job_executor(event_loop)
        .build()
        .map_err(|err| JsNativeError::error().with_message(err.to_string()))?;
    if let Some(start) = realm.worker.as_deref_mut() {
        bun_runtime_js::web::worker::enter(&mut context, start);
    }
    loader.install(&mut context);
    boa_runtime::register(
        boa_runtime::extensions::ConsoleExtension(boa_runtime::DefaultLogger),
        None,
        &mut context,
    )?;
    bun_runtime_js::web::performance::start();
    bun_runtime_js::install(&mut context)?;
    let ops = native(&mut context, &realm)?;
    context.register_global_property(js_string!("__bun_native_ops__"), ops, Attribute::all())?;
    let worker_ops = bun_runtime_js::web::message_port::ops(&mut context)?;
    context.register_global_property(js_string!("__bun_worker_ops__"), worker_ops, Attribute::all())?;
    // EventTarget and the events first: performance.ts extends EventTarget, worker.ts the rest.
    for (name, source) in [
        (&b"bun:events.ts"[..], &include_bytes!("js/events.ts")[..]),
        (b"bun:performance.ts", include_bytes!("js/performance.ts")),
        (b"bun:crypto.ts", include_bytes!("js/crypto.ts")),
        (b"bun:prelude.ts", include_bytes!("js/bun.ts")),
        (b"bun:worker.ts", include_bytes!("js/worker.ts")),
    ] {
        let js = crate::transpile::transpile(name, source).map_err(|err| JsNativeError::syntax().with_message(err))?;
        context.eval(Source::from_bytes(&js))?;
    }
    crate::builtins::install(&mut context)?;
    Ok(context)
}
