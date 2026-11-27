//! `fs.Stats`, `fs.BigIntStats` and `fs.Dirent` (src/runtime/node/Stat.rs and the `Dirent` of types.rs): the
//! classes are stats.js, made once per realm, and Rust makes their instances with what a stat call returns.

use bun_js::bindgen::JsScope;
use bun_js::{JSGlobalObject, JSValue, JsResult};
use bun_sys::PosixStat;

use crate::generated_bindgen::stat::Ino;

struct Classes;

/// `{ Stats, BigIntStats, Dirent, stats, bigIntStats, dirent }` of stats.js.
fn classes(global: &JSGlobalObject) -> JsResult<JSValue> {
    global.cached::<Classes>(|global| global.evaluate_script(include_str!("stats.js")))
}

fn member(global: &JSGlobalObject, name: &str) -> JsResult<JSValue> {
    Ok(classes(global)?.get(global, name)?.unwrap_or_default())
}

/// The constructor of `Stats`, `BigIntStats` or `Dirent`. stats.js defines them without running anything else: it
/// does not throw.
pub(crate) fn constructor(global: &JSGlobalObject, name: &str) -> JSValue {
    member(global, name).expect("stats.js defines the classes")
}

/// `(sec, nsec)` of a time, as `StatType::timespec_parts`.
fn parts(time: bun_core::Timespec) -> (i64, i64) {
    (time.sec, time.nsec)
}

fn ms(time: bun_core::Timespec) -> f64 {
    let (sec, nsec) = parts(time);
    (sec as f64) * 1000.0 + (nsec as f64) / 1_000_000.0
}

fn ns(time: bun_core::Timespec) -> i128 {
    let (sec, nsec) = parts(time);
    i128::from(sec) * 1_000_000_000 + i128::from(nsec)
}

/// `StatType::<BIG>::to_js`.
pub(crate) fn to_js(global: &JSGlobalObject, stat: &PosixStat, big_int: bool) -> JsResult<JSValue> {
    let integers = [
        stat.dev, stat.ino, stat.mode, stat.nlink, stat.uid, stat.gid, stat.rdev, stat.size, stat.blksize,
        stat.blocks,
    ];
    let times = [stat.atim, stat.mtim, stat.ctim, stat.birthtim];
    let mut args: Vec<JSValue> = Vec::with_capacity(14);
    if big_int {
        args.extend(integers.iter().map(|&n| JSValue::from_i128(i128::from(n as i64))));
        args.extend(times.iter().map(|&t| JSValue::from_i128(ns(t))));
        return member(global, "bigIntStats")?.call(global, JSValue::UNDEFINED, &args);
    }
    args.extend(integers.iter().map(|&n| JSValue::js_number(n as f64)));
    args.extend(times.iter().map(|&t| JSValue::js_number(ms(t))));
    member(global, "stats")?.call(global, JSValue::UNDEFINED, &args)
}

/// `createStatsForIno` of bun:internal-for-testing: the stats of a zeroed stat but for `ino`.
pub(crate) fn create_stats_for_ino(cx: &mut JsScope<'_>, ino: Ino, big: bool) -> JsResult<JSValue> {
    let mut stat: PosixStat = bun_core::ffi::zeroed();
    stat.ino = match ino {
        Ino::Number(number) => to_uint64_no_truncate(number),
        Ino::Bigint(ino) => ino,
    };
    to_js(cx.global(), &stat, big)
}

/// `JSValue::to_uint64_no_truncate` of a number: 2^64 and more saturate, the rest is `JSC::toUInt64`, modulo 2^64.
fn to_uint64_no_truncate(number: f64) -> u64 {
    const TWO_TO_64: f64 = 18446744073709551616.0;
    if number >= TWO_TO_64 {
        return u64::MAX;
    }
    number.trunc().rem_euclid(TWO_TO_64) as u64
}

/// What `Dirent` says of an entry's kind, uv's `uv_dirent_type_t`.
pub(crate) fn dirent_type(kind: bun_sys::EntryKind) -> i32 {
    use bun_sys::EntryKind as K;
    match kind {
        K::File => 1,
        K::Directory => 2,
        K::SymLink => 3,
        K::NamedPipe => 4,
        K::UnixDomainSocket => 5,
        K::CharacterDevice => 6,
        K::BlockDevice => 7,
        _ => 0,
    }
}

/// `new Dirent(name, type, parentPath)`, as `Bun__Dirent__toJS`.
pub(crate) fn dirent(global: &JSGlobalObject, name: JSValue, kind: i32, path: JSValue) -> JsResult<JSValue> {
    member(global, "dirent")?.call(
        global,
        JSValue::UNDEFINED,
        &[name, JSValue::js_number_from_int32(kind), path],
    )
}
