//! TypeScript/JSX → JavaScript with Bun's parser and printer, the way `bun run` transpiles a file.

use bun_alloc::Arena;
use bun_ast::Loader;

pub fn loader_for(path: &[u8]) -> Loader {
    let ext = path.rsplit(|&b| b == b'.').next().unwrap_or_default();
    match ext {
        b"ts" | b"mts" | b"cts" => Loader::Ts,
        b"tsx" => Loader::Tsx,
        _ => Loader::Jsx,
    }
}

/// The parser allocates AST nodes from thread-local stores; they must outlive the print.
struct StoreScope(bun_ast::StoreAstAllocHeap);

impl StoreScope {
    fn enter() -> StoreScope {
        bun_ast::expr::data::Store::create();
        bun_ast::stmt::data::Store::create();
        StoreScope(bun_ast::StoreAstAllocHeap::new())
    }
}

impl Drop for StoreScope {
    fn drop(&mut self) {
        bun_ast::expr::data::Store::deinit();
        bun_ast::stmt::data::Store::deinit();
    }
}

fn messages(log: &bun_ast::Log) -> String {
    let mut out = String::new();
    let _ = log.print(&mut out);
    out
}

pub fn transpile(path: &[u8], contents: &[u8]) -> Result<Vec<u8>, String> {
    transpile_module(path, contents).map(|module| module.code)
}

pub struct Transpiled {
    pub code: Vec<u8>,
    /// A CommonJS module, printed as `(function (exports, require, module, __filename, __dirname[, importMeta]) { … })`.
    pub commonjs: bool,
    /// From the printed code back to `contents`.
    pub source_map: Option<bun_sourcemap::ParsedSourceMap>,
}

/// As Bun's runtime transpiles a module: an ES module that names `require`, `__filename` or `__dirname` reads
/// them from `import.meta`, and a CommonJS module is a function the loader calls.
pub fn transpile_module(path: &[u8], contents: &[u8]) -> Result<Transpiled, String> {
    transpile_printing::<false>(path, contents)
}

/// As [`transpile_module`], the same code, with its source map: what `source_maps` makes once a position of the
/// file is asked for, so that a module that throws nothing costs no map.
pub fn transpile_with_source_map(path: &[u8], contents: &[u8]) -> Result<Transpiled, String> {
    transpile_printing::<true>(path, contents)
}

fn transpile_printing<const SOURCE_MAP: bool>(path: &[u8], contents: &[u8]) -> Result<Transpiled, String> {
    let loader = loader_for(path);
    let _store = StoreScope::enter();
    let arena = Arena::new();
    let source = bun_ast::Source::init_path_string(path, contents);

    let mut opts = bun_js_parser::ParserOptions::init(Default::default(), loader);
    opts.features.top_level_await = true;
    opts.features.standard_decorators = !loader.is_typescript();
    // The runtime transpiler's default (src/bundler/transpiler.rs): TypeScript drops the imports it does not use,
    // such as those of types only.
    opts.features.trim_unused_imports = loader.is_typescript();
    opts.transform_only = true;
    opts.features.commonjs_at_runtime = true;
    let define: &bun_js_parser::Define = arena.alloc(bun_js_parser::Define::default());
    let mut log = bun_ast::Log::init();
    let parser = bun_js_parser::Parser::init(opts, &mut log, &source, define, &arena)
        .map_err(|_| messages(&log))?;
    let mut ast = match parser.parse() {
        Ok(bun_js_parser::Result::Ast(ast)) if log.errors == 0 => ast,
        _ => return Err(messages(&log)),
    };

    let commonjs = ast.exports_kind == bun_ast::ExportsKind::Cjs;
    let alloc = *ast.symbols.allocator();
    let list = core::mem::replace(&mut ast.symbols, bun_alloc::ArenaVec::new_in(alloc));
    let symbols = bun_ast::symbol::Map::init_with_one_list(list.into_iter().collect());
    let mut printer = bun_js_printer::BufferPrinter::init(bun_js_printer::BufferWriter::init());
    let chunk = core::cell::Cell::new(None);
    let options = bun_js_printer::Options {
        require_ref: Some(ast.require_ref),
        import_meta_ref: ast.import_meta_ref,
        // As src/bundler/transpiler.rs: a CommonJS module reads `import.meta` from its wrapper's parameter.
        module_type: if commonjs {
            bun_options_types::bundle_enums::Format::Cjs
        } else {
            bun_options_types::bundle_enums::Format::Esm
        },
        target: bun_ast::Target::Bun,
        source_map_handler: SOURCE_MAP.then_some(&chunk as &dyn bun_js_printer::OnSourceMapChunk),
        ..Default::default()
    };
    bun_js_printer::print_ast::<false, SOURCE_MAP>(
        &mut printer,
        &arena,
        &ast,
        symbols,
        &source,
        options,
    )
    .map_err(|err| format!("{}: {err}", String::from_utf8_lossy(path)))?;
    // The positions of the printed code, mapped back to the file as it is written (`;`-separated VLQ mappings).
    let source_map = chunk.take().and_then(|chunk: bun_sourcemap::Chunk| {
        let options = bun_sourcemap::mapping::ParseOptions { allow_names: false, sort: true };
        bun_sourcemap::mapping::parse(chunk.buffer.list.as_slice(), None, 1, options).ok()
    });
    Ok(Transpiled {
        code: printer.ctx.buffer.list,
        commonjs,
        source_map,
    })
}
