//! `bun build <entry…> [--outdir dir | --outfile file] [--minify] [--no-bundle]`: Bun's bundler (`BundleV2::generate_from_cli`)
//! driven the way `src/runtime/cli/build_command.rs` drives it, without `bun_runtime`'s CLI context.
//! Behind the `build` feature until the bundler's graph links without C/C++.

use bun_bundler::bundle_v2::BundleV2;
use bun_bundler::{options, transpiler};
use bun_options_types::schema::api;

pub fn main(args: &[String]) -> i32 {
    bun_core::output::stdio::init();
    let (mut entries, mut outdir, mut outfile, mut minify): (_, Option<String>, Option<String>, _) =
        (Vec::new(), None, None, false);
    let mut no_bundle = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--outdir" | "--outfile" => {
                let Some(value) = it.next().filter(|v| !v.starts_with('-')) else {
                    eprintln!("error: {arg} needs a value");
                    return 1;
                };
                *(if arg == "--outdir" {
                    &mut outdir
                } else {
                    &mut outfile
                }) = Some(value.clone());
            }
            "--minify" => minify = true,
            "--no-bundle" => no_bundle = true,
            flag if flag.starts_with("--outdir=") => {
                outdir = Some(flag["--outdir=".len()..].into())
            }
            flag if flag.starts_with("--outfile=") => {
                outfile = Some(flag["--outfile=".len()..].into())
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: this bun's `bun build` does not support {flag} yet");
                return 2;
            }
            entry => entries.push(Box::<[u8]>::from(entry.as_bytes())),
        }
    }
    if outdir.is_some() && outfile.is_some() {
        eprintln!("error: Cannot use both --outdir and --outfile");
        return 1;
    }
    if entries.is_empty() {
        eprintln!("error: Missing entrypoints. What would you like to bundle?");
        return 1;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_bytes = cwd.as_os_str().as_encoded_bytes().to_vec();
    bun_paths::fs::FileSystem::init(&cwd_bytes);
    let arena: &'static bun_alloc::Arena = Box::leak(Box::new(bun_alloc::Arena::new()));
    let log: *mut bun_ast::Log = Box::leak(Box::new(bun_ast::Log::init()));
    let opts = api::TransformOptions {
        entry_points: entries.clone(),
        absolute_working_dir: Some(cwd_bytes.clone().into()),
        ..Default::default()
    };
    let this: &'static mut transpiler::Transpiler<'static> =
        match transpiler::Transpiler::init(arena, log, opts, None) {
            Ok(t) => arena.alloc(t),
            Err(err) => {
                eprintln!("error: {}", err.name());
                return 1;
            }
        };
    this.options.entry_points = entries.clone().into();
    this.options.minify_identifiers = minify;
    this.options.minify_whitespace = minify;
    this.options.minify_syntax = minify;
    if outdir.is_none() && entries.len() > 1 {
        eprintln!("error: Must use --outdir when specifying more than one entry point.");
        return 1;
    }
    // As build_command.rs: --outfile names the entry chunk, and its directory is the outdir of everything else
    // (sourcemaps, assets), so no output is dropped or sent to stdout.
    if let Some(file) = &outfile {
        let path = std::path::Path::new(file);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.clone());
        this.options.entry_naming = format!("./{name}").into_bytes().into();
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        outdir = Some(dir.to_string_lossy().into_owned());
    }
    // No outdir: an empty output_dir keeps the chunks in memory, for stdout.
    this.options.output_dir = outdir.as_deref().unwrap_or("").as_bytes().into();
    this.options.root_dir = options::default_root_dir(&this.options.entry_points).into();
    if this.configure_defines().is_err() {
        eprintln!("error: could not configure defines");
        return 1;
    }
    this.configure_linker();
    this.options.transform_only = no_bundle;
    this.sync_resolver_opts();
    // As build_command.rs: --no-bundle transpiles each entry without linking, through Transpiler::transform.
    if no_bundle {
        this.options.import_path_format = options::ImportPathFormat::Relative;
        this.options.allow_runtime = false;
        this.resolver.opts.allow_runtime = false;
        let result = this.transform(log, api::TransformOptions::default());
        // SAFETY: leaked above, and the transform is done with it.
        let log = unsafe { &*log };
        if log.has_errors() {
            let mut text = String::new();
            let _ = log.print(&mut text);
            eprint!("{text}");
        }
        let output = match result {
            Ok(result) if result.errors.is_empty() && !result.output_files.is_empty() => {
                result.output_files
            }
            Ok(_) => return 1,
            Err(err) => {
                eprintln!("error: {}", err.name());
                return 1;
            }
        };
        return write_outputs(&output, outdir.as_deref());
    }
    let event_loop = bun_event_loop::AnyEventLoop::init();
    let (mut files, mut minify_ns, mut size) = (0usize, 0u64, 0u64);
    let result = BundleV2::generate_from_cli(
        this,
        arena,
        Some(core::ptr::NonNull::from(&event_loop)),
        &mut files,
        &mut minify_ns,
        &mut size,
        None,
    );
    let print_log = || {
        let mut text = String::new();
        // SAFETY: leaked above, and the bundle is done with it.
        let _ = unsafe { (*log).print(&mut text) };
        eprint!("{text}");
    };
    let output = match result {
        Ok(result) => result.output_files,
        Err(err) => {
            print_log();
            eprintln!("error: {}", err.name());
            return 1;
        }
    };
    write_outputs(&output, outdir.as_deref())
}

fn write_outputs(output: &[options::OutputFile], outdir: Option<&str>) -> i32 {
    for file in output.iter() {
        let options::OutputFileValue::Buffer { bytes } = &file.value else {
            continue;
        };
        let dest = match outdir {
            Some(dir) => {
                std::path::Path::new(dir).join(String::from_utf8_lossy(&file.dest_path).as_ref())
            }
            None => {
                use std::io::Write;
                let _ = std::io::stdout().write_all(bytes);
                continue;
            }
        };
        // make_open creates the parent directories.
        let flags = bun_sys::O::WRONLY | bun_sys::O::CREAT | bun_sys::O::TRUNC;
        if let Err(err) =
            bun_sys::File::make_open(dest.as_os_str().as_encoded_bytes(), flags, 0o664)
                .and_then(|f| f.write_all(bytes))
        {
            eprintln!("error: {err}");
            return 1;
        }
    }
    0
}

/// The parser asks the macro VM to collect garbage after a file; there is no macro VM here.
#[unsafe(no_mangle)]
extern "Rust" fn __bun_macro_collect_vm_garbage() {}
