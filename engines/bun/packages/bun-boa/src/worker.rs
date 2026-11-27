//! The body of a worker's thread (src/jsc/web_worker.rs's `start`/`spin`), for `bun_runtime_js::web::worker`: a
//! realm with Bun's globals, `node:worker_threads` for a node worker, the preloads, the entry module, then the event
//! loop until nothing keeps it alive or the worker is terminated. And where a worker's specifier leads, which the
//! constructor resolves before a thread starts.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::{Context, JsError, JsNativeError, JsResult, JsValue, Module, Source, js_string};
use bun_runtime_js::EventLoop;
use bun_runtime_js::web::worker::{Entry, Host, Reporter, WorkerStart};

use crate::loader::BunModuleLoader;

pub fn install() {
    bun_runtime_js::web::worker::set_host(Host { resolve, run });
}

fn type_error(message: String) -> JsError {
    JsNativeError::typ().with_message(message).into()
}

/// `data:[<type>][;base64],<data>`: the bytes of its data.
fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let comma = bun_core::strings::index_of_char_usize(rest.as_bytes(), b',')?;
    let (meta, data) = (&rest[..comma], &rest[comma + 1..]);
    let bytes = percent_decode(data.as_bytes());
    if meta.ends_with(";base64") {
        let text: Vec<u8> = bytes.into_iter().filter(|b| !b.is_ascii_whitespace()).collect();
        return bun_base64::decode_alloc(&text).ok();
    }
    Some(bytes)
}

fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(Some(high)), Some(Some(low))) = (bytes.get(i + 1).map(|&b| hex(b)), bytes.get(i + 2).map(|&b| hex(b)))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Worker.cpp's `Worker::create` and web_worker.rs's `resolve_entry_point_specifier`: a `file:` URL is its path, a
/// `data:` or `blob:` URL is the module it holds, anything else resolves as `bun run`'s entry point does.
fn resolve(specifier: &str, _context: &mut Context) -> JsResult<Entry> {
    if specifier.starts_with("data:") {
        let source = decode_data_url(specifier).ok_or_else(|| type_error(format!("Invalid data URL: \"{specifier}\"")))?;
        return Ok(Entry { specifier: specifier.to_owned(), source: Some(source) });
    }
    if bun_runtime_js::webcore::object_url_registry::is_blob_url(specifier.as_bytes()) {
        let blob = bun_runtime_js::webcore::object_url_registry::resolve(specifier.as_bytes())
            .ok_or_else(|| type_error("Blob URL is missing".to_owned()))?;
        let source = blob.read_bytes().map_err(|_| type_error(format!("Blob URL could not be read: \"{specifier}\"")))?;
        return Ok(Entry { specifier: specifier.to_owned(), source: Some(source) });
    }
    let path = if specifier.starts_with("file://") {
        let invalid = || type_error(format!("Invalid file URL: \"{specifier}\""));
        let url = url::Url::parse(specifier).map_err(|_| invalid())?;
        url.to_file_path().map_err(|_| invalid())?
    } else {
        PathBuf::from(specifier)
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let text = path.to_string_lossy();
    let resolved = crate::loader::resolve(&text, &cwd)
        .or_else(|| path.is_absolute().then(|| path.canonicalize().ok()).flatten())
        .ok_or_else(|| type_error(format!("BuildMessage: ModuleNotFound resolving \"{text}\" (entry point)")))?;
    Ok(Entry { specifier: resolved.display().to_string(), source: None })
}

/// The module of `entry` in this realm.
fn load(entry: &Entry, loader: &BunModuleLoader, context: &mut Context) -> JsResult<Module> {
    let Some(source) = &entry.source else { return loader.load(Path::new(&entry.specifier), context) };
    // As Bun's for a `data:` or `blob:` entry: JavaScript, an ES module or CommonJS, relative to the working directory.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::current_dir().unwrap_or_default().join(format!("[worker {n}].js"));
    loader.load_source(&path, source, context)
}

enum Outcome {
    Done,
    /// The loop ran out with the module's evaluation still pending (a top-level await that never settles).
    Pending,
}

/// Loads, links and evaluates `module`, running the loop until its evaluation settles.
fn evaluate(module: Module, event_loop: &EventLoop, context: &mut Context) -> JsResult<Outcome> {
    let promise = module.load_link_evaluate(context);
    event_loop.run_until(context, || !matches!(promise.state(), PromiseState::Pending))?;
    match promise.state() {
        PromiseState::Fulfilled(_) => Ok(Outcome::Done),
        PromiseState::Rejected(reason) => Err(JsError::from_opaque(reason)),
        PromiseState::Pending => Ok(Outcome::Pending),
    }
}

/// `{ name, message, stack }` of what was thrown, as strings.
fn describe(error: JsError, context: &mut Context) -> (String, String, String) {
    let value = error.into_opaque(context).unwrap_or_else(|e| JsValue::from(js_string!(e.to_string())));
    let field = |name: &str, context: &mut Context| -> String {
        value
            .as_object()
            .and_then(|o| o.get(boa_engine::JsString::from(name), context).ok())
            .filter(|v| !v.is_undefined())
            .map(|v| v.display().to_string().trim_matches('"').to_owned())
            .unwrap_or_default()
    };
    if value.is_object() {
        (field("name", context), field("message", context), field("stack", context))
    } else {
        (String::new(), value.display().to_string(), String::new())
    }
}

fn process_exit_code(context: &mut Context) -> Option<i32> {
    let process = context.global_object().get(js_string!("process"), context).ok()?;
    let code = process.as_object()?.get(js_string!("exitCode"), context).ok()?;
    code.as_number().map(|code| code as i32)
}

fn run(mut start: WorkerStart, report: &Reporter) -> i32 {
    let control = start.control.clone();
    let (main, specifier) = match &start.options.entry.source {
        Some(_) => (None, start.options.entry.specifier.clone()),
        None => (Some(PathBuf::from(&start.options.entry.specifier)), start.options.entry.specifier.clone()),
    };
    let argv = [std::env::args().next().unwrap_or_else(|| "bun".into()), specifier]
        .into_iter()
        .chain(start.options.argv.iter().cloned())
        .collect();
    let env = start.options.env.take();
    let preload = std::mem::take(&mut start.options.preload);
    let entry = Entry { specifier: start.options.entry.specifier.clone(), source: start.options.entry.source.take() };
    let node = start.options.node;
    let loader = Rc::new(BunModuleLoader::default());
    let realm = crate::runtime::Realm { main, argv, env, worker: Some(&mut start) };
    let mut context = match crate::runtime::context_for(loader.clone(), realm) {
        Ok(context) => context,
        Err(error) => {
            report.error("Error".into(), error.to_string(), String::new());
            return 1;
        }
    };
    let event_loop = EventLoop::of(&context);
    context.set_interrupt(Some(event_loop.terminating()));
    control.attach(event_loop.remote(), event_loop.terminating());
    report.online();

    let result = (|| -> JsResult<Outcome> {
        if node
            && let Some(id) = crate::builtins::id_of("node:worker_threads")
        {
            crate::builtins::require(id, &mut context)?;
        }
        for preload in &preload {
            let module = load(preload, &loader, &mut context)?;
            if let Outcome::Pending = evaluate(module, &event_loop, &mut context)? {
                return Ok(Outcome::Pending);
            }
        }
        let module = load(&entry, &loader, &mut context)?;
        let outcome = evaluate(module, &event_loop, &mut context)?;
        bun_runtime_js::web::message_port::entry_evaluated(&mut context);
        // node:worker_threads' hook (`setEntryEvaluatedHook`), which the messaging of postMessageToThread waits for.
        context.eval(Source::from_bytes("globalThis[Symbol.for('bun.boa.worker')]?.entryEvaluated()"))?;
        if let Outcome::Pending = outcome {
            return Ok(Outcome::Pending);
        }
        event_loop.run(&mut context)?;
        Ok(Outcome::Done)
    })();

    let code = match result {
        _ if let Some(code) = bun_runtime_js::web::worker::exit_code(&context) => code,
        _ if control.terminated() => 1,
        Ok(Outcome::Done) => process_exit_code(&mut context).unwrap_or(0),
        // Node's: 13, unless the script set another code.
        Ok(Outcome::Pending) => process_exit_code(&mut context).filter(|&code| code != 0).unwrap_or(13),
        Err(error) if bun_runtime_js::event_loop::is_termination(&error) => 1,
        Err(error) => {
            let (name, message, stack) = describe(error, &mut context);
            report.error(name, message, stack);
            1
        }
    };
    event_loop.close_remote();
    code
}
