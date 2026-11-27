//! The source maps of the modules the loader transpiled: Boa's stack positions are those of the printed code, and
//! these map them back to the file as it is written, for error output and for where `toMatchInlineSnapshot()` is
//! called (Bun's `SavedSourceMap`). A map is made the first time a position of its file is asked for, by printing
//! the file again with one: the printer gives the same code as the loader ran.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bun_sourcemap::{Ordinal, ParsedSourceMap};

struct Saved {
    map: ParsedSourceMap,
    source: Rc<str>,
}

thread_local! {
    /// `None` for a file that has no map: it cannot be read, or does not transpile.
    static SAVED: RefCell<HashMap<String, Option<Rc<Saved>>>> = RefCell::new(HashMap::new());
}

fn saved(path: &str) -> Option<Rc<Saved>> {
    if let Some(saved) = SAVED.with_borrow(|saved| saved.get(path).cloned()) {
        return saved;
    }
    let made = (|| {
        let contents = bun_sys::File::read_from(bun_sys::Fd::cwd(), path.as_bytes()).ok()?;
        let map = crate::transpile::transpile_with_source_map(path.as_bytes(), &contents).ok()?.source_map?;
        Some(Rc::new(Saved { map, source: Rc::from(String::from_utf8_lossy(&contents).as_ref()) }))
    })();
    SAVED.with_borrow_mut(|saved| saved.insert(path.to_owned(), made.clone()));
    made
}

/// The position in the file as it is written of the 1-based `line` and `column` of the printed code.
pub fn original_position(path: &str, line: u32, column: u32) -> Option<(u32, u32)> {
    let saved = saved(path)?;
    let mapping = saved.map.find_mapping(
        Ordinal::from_one_based(i32::try_from(line).ok()?),
        Ordinal::from_one_based(i32::try_from(column).ok()?),
    )?;
    Some(((mapping.original_line() + 1) as u32, (mapping.original_column() + 1) as u32))
}

/// The text of `path` as it is written, if the loader transpiles it.
pub fn original_source(path: &str) -> Option<Rc<str>> {
    saved(path).map(|saved| Rc::clone(&saved.source))
}
