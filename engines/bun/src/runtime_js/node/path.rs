//! The bodies of src/runtime/node/node_path.bind.ts on `bun_js`: src/runtime/node/path.rs without the forms of a
//! JavaScriptCore string it reads in place. An operand arrives as UTF-8, and `bun_node_path`'s functions on bytes answer.

use bun_js::JSValue;
use bun_js::bindgen::{self, JsHeap, JsLocal, JsScope};
use bun_node_path::{
    CHAR_BACKWARD_SLASH, CHAR_FORWARD_SLASH, PathParsed, basename_posix_t, basename_windows_t,
    dirname_posix_t, dirname_windows_t, extname_posix_t, extname_windows_t, format_t,
    is_absolute_posix_t, is_absolute_windows_t, join_buf_len, join_posix_t, join_windows_t,
    normalize_buf_len, normalize_posix_t, normalize_windows_t, parse_posix_t, parse_windows_t,
    path_size, relative_buf_len, relative_posix_t, relative_windows_t, resolve_buf_len,
    resolve_posix_t, resolve_windows_t, to_namespaced_path_buf_len, to_namespaced_path_windows_t,
};

use crate::generated_bindgen::node_path::ParsedPath;
use crate::node::validators::{validate_object, validate_string};

type Text<'a> = &'a [u8];
type TextResult<'a> = bindgen::Result<'a, Vec<u8>>;

/// How long UTF-8 `text` is in UTF-16 code units, which Node measures a suffix in.
fn utf16_length(text: &[u8]) -> usize {
    text.iter()
        .map(|&byte| match byte {
            0x80..=0xBF => 0,
            0xF0..=0xFF => 2,
            _ => 1,
        })
        .sum()
}

// ───────────────────────────── parts of one path ─────────────────────────────

fn basename<'a, const WINDOWS: bool>(path: Text<'a>, suffix: Option<Text<'a>>) -> Text<'a> {
    let suffix =
        suffix.filter(|suffix| !suffix.is_empty() && utf16_length(suffix) <= utf16_length(path));
    if WINDOWS {
        basename_windows_t(path, suffix)
    } else {
        basename_posix_t(path, suffix)
    }
}

fn dirname<const WINDOWS: bool>(path: Text<'_>) -> Text<'_> {
    if WINDOWS {
        dirname_windows_t(path)
    } else {
        dirname_posix_t(path)
    }
}

fn extname<const WINDOWS: bool>(path: Text<'_>) -> Text<'_> {
    if WINDOWS {
        extname_windows_t(path)
    } else {
        extname_posix_t(path)
    }
}

fn is_absolute<const WINDOWS: bool>(path: Text<'_>) -> bool {
    if WINDOWS {
        is_absolute_windows_t(path)
    } else {
        is_absolute_posix_t(path)
    }
}

fn parse<const WINDOWS: bool>(path: Text<'_>) -> ParsedPath<'_> {
    let parsed = if WINDOWS {
        parse_windows_t(path)
    } else {
        parse_posix_t(path)
    };
    ParsedPath {
        root: parsed.root,
        dir: parsed.dir,
        base: parsed.base,
        ext: parsed.ext,
        name: parsed.name,
    }
}

// ───────────────────────────── paths that are made ─────────────────────────────

fn normalize<const WINDOWS: bool>(path: Text<'_>) -> Vec<u8> {
    let mut buf = vec![0; normalize_buf_len(path)];
    let normalized = if WINDOWS {
        normalize_windows_t(path, &mut buf)
    } else {
        normalize_posix_t(path, &mut buf)
    };
    normalized.to_vec()
}

fn join<const WINDOWS: bool>(paths: &[Text<'_>]) -> Vec<u8> {
    let parts: Vec<&[u8]> = paths.iter().copied().filter(|path| !path.is_empty()).collect();
    let buf_len = join_buf_len(WINDOWS, &parts);
    let mut scratch = vec![0; buf_len * 2];
    let (buf, buf2) = scratch.split_at_mut(buf_len);
    let joined = if WINDOWS {
        join_windows_t(&parts, buf, buf2)
    } else {
        join_posix_t(&parts, buf, buf2)
    };
    joined.to_vec()
}

fn relative<const WINDOWS: bool>(from: Text<'_>, to: Text<'_>) -> Vec<u8> {
    if from.is_empty() && to.is_empty() {
        return Vec::new();
    }
    let buf_len = relative_buf_len(from, to);
    let mut scratch = vec![0; buf_len * 3];
    let (buf, rest) = scratch.split_at_mut(buf_len);
    let (from_buf, tmp_buf) = rest.split_at_mut(buf_len);
    let relative = if WINDOWS {
        relative_windows_t(from, to, buf, from_buf, tmp_buf)
    } else {
        relative_posix_t(from, to, buf, from_buf, tmp_buf)
    };
    relative.to_vec()
}

/// From the last path to the first, only up to one that is absolute on POSIX: only those have to be strings. The
/// working directory completes what is not absolute, `bun_node_path`'s.
fn resolve<'a, const WINDOWS: bool>(heap: &JsHeap<'a>, paths: &[JsLocal<'a>]) -> TextResult<'a> {
    let mut texts: Vec<String> = Vec::new();
    for (index, path) in paths.iter().enumerate().rev() {
        validate_string(heap, path.value(), format_args!("paths[{index}]"))?;
        let Some(text) = path.value().as_string() else {
            unreachable!("a String object arrives as a string");
        };
        if text.is_empty() {
            continue;
        }
        let is_absolute = !WINDOWS && text.as_bytes()[0] == CHAR_FORWARD_SLASH;
        texts.push(text);
        if is_absolute {
            break;
        }
    }
    let parts: Vec<&[u8]> = texts.iter().rev().map(String::as_bytes).collect();
    let buf_len = resolve_buf_len(WINDOWS, &parts);
    let mut scratch = vec![0; buf_len * 2];
    let (buf, buf2) = scratch.split_at_mut(buf_len);
    let resolved = if WINDOWS {
        resolve_windows_t(&parts, buf, buf2)
    } else {
        resolve_posix_t(&parts, buf, buf2)
    };
    Ok(resolved.to_vec())
}

/// What `bun_jsc`'s `get_truthy` keeps of a member: anything but `undefined`, `null` and the empty string.
fn is_given(value: &JSValue) -> bool {
    !value.is_undefined_or_null() && !(value.is_string() && !value.to_boolean())
}

/// Reads `root`, `dir`, `base`, `name` and `ext`, in that order, and `ToString`s each that is given as it reads it.
fn format<'a, const WINDOWS: bool>(cx: &mut JsScope<'a>, path_object: JsLocal<'a>) -> TextResult<'a> {
    let separator = if WINDOWS {
        CHAR_BACKWARD_SLASH
    } else {
        CHAR_FORWARD_SLASH
    };
    let heap = &cx.heap;
    let global = heap.global_object();
    let path_object = path_object.value();
    validate_object(heap, path_object, "pathObject")?;
    let mut members: [Option<String>; 5] = Default::default();
    for (member, name) in members
        .iter_mut()
        .zip(["root", "dir", "base", "name", "ext"])
    {
        if let Some(value) = path_object.get(global, name)?.filter(is_given) {
            *member = Some(value.to_utf8(global)?);
        }
    }
    let [root, dir, base, name, ext] = members
        .each_ref()
        .map(|member| member.as_deref().map_or(&b""[..], str::as_bytes));
    // One more for the separator.
    let buf_len = (1
        + (if dir.is_empty() { root.len() } else { dir.len() })
        + (if base.is_empty() {
            name.len() + ext.len() + 1
        } else {
            base.len()
        }))
    .max(path_size::<u8>());
    let mut buf = vec![0; buf_len];
    let parsed = PathParsed {
        root,
        dir,
        base,
        ext,
        name,
    };
    Ok(format_t(&parsed, separator, &mut buf).to_vec())
}

// ───────────────────────────── what node_path.bind.ts names ─────────────────────────────

macro_rules! on_both_platforms {
    ($( $posix:ident, $win32:ident = $body:ident($($parameter:ident: $type:ty),*) -> $result:ty; )*) => {$(
        pub(crate) fn $posix<'a>($($parameter: $type),*) -> $result {
            $body::<false>($($parameter),*)
        }
        pub(crate) fn $win32<'a>($($parameter: $type),*) -> $result {
            $body::<true>($($parameter),*)
        }
    )*};
}
on_both_platforms! {
    format_posix, format_win32 = format(cx: &mut JsScope<'a>, path_object: JsLocal<'a>) -> TextResult<'a>;
    resolve_posix, resolve_win32 = resolve(heap: &JsHeap<'a>, paths: &[JsLocal<'a>]) -> TextResult<'a>;
    is_absolute_posix, is_absolute_win32 = is_absolute(path: Text<'a>) -> bool;
}

/// What a body that cannot fail but is said to throw (`throws: true`) answers on both platforms.
macro_rules! infallible_on_both_platforms {
    ($( $posix:ident, $win32:ident = $body:ident($($parameter:ident: $type:ty),*) -> $result:ty; )*) => {$(
        pub(crate) fn $posix<'a>(_: &JsHeap<'a>, $($parameter: $type),*) -> bindgen::Result<'a, $result> {
            Ok($body::<false>($($parameter),*))
        }
        pub(crate) fn $win32<'a>(_: &JsHeap<'a>, $($parameter: $type),*) -> bindgen::Result<'a, $result> {
            Ok($body::<true>($($parameter),*))
        }
    )*};
}
infallible_on_both_platforms! {
    basename_posix, basename_win32 = basename(path: Text<'a>, ext: Option<Text<'a>>) -> Text<'a>;
    dirname_posix, dirname_win32 = dirname(path: Text<'a>) -> Text<'a>;
    extname_posix, extname_win32 = extname(path: Text<'a>) -> Text<'a>;
    join_posix, join_win32 = join(paths: &[Text<'a>]) -> Vec<u8>;
    normalize_posix, normalize_win32 = normalize(path: Text<'a>) -> Vec<u8>;
    parse_posix, parse_win32 = parse(path: Text<'a>) -> ParsedPath<'a>;
    relative_posix, relative_win32 = relative(from: Text<'a>, to: Text<'a>) -> Vec<u8>;
}

pub(crate) fn to_namespaced_path_posix<'a>(_: &JsHeap<'a>, path: JsLocal<'a>) -> JsLocal<'a> {
    path
}

/// Based on Node v21.6.1 path.win32.toNamespacedPath: https://github.com/nodejs/node/blob/6ae20aa63de78294b18d5015481485b7cd8fbb60/lib/path.js#L624
/// A `String` object counts as a string, and one whose string is empty is answered as it is.
pub(crate) fn to_namespaced_path_win32<'a>(
    cx: &mut JsScope<'a>,
    path: JsLocal<'a>,
) -> bindgen::Result<'a, JsLocal<'a>> {
    let value = path.value();
    if !value.is_string() && !value.is_string_object() {
        return Ok(path);
    }
    let heap = &cx.heap;
    let text = value.to_utf8(heap.global_object())?;
    if text.is_empty() {
        return Ok(path);
    }
    let text = text.as_bytes();
    let buf_len = to_namespaced_path_buf_len(text);
    let mut scratch = vec![0; buf_len * 2];
    let (buf, buf2) = scratch.split_at_mut(buf_len);
    heap.create_utf8_string(to_namespaced_path_windows_t(text, buf, buf2))
}
