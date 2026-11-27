//! The link-time `extern "Rust"` symbols that `bun_install`'s graph declares and `bun_runtime` defines, for a binary
//! without `bun_runtime`. The package manager runs on its `MiniEventLoop`, so what only a JavaScript VM's loop has
//! (its timer heap, `jsc::EventLoop`, its `EventLoopCtx`) is never reached here.

use core::ptr::NonNull;

use bun_core::String as BunString;
use bun_event_loop::EventLoopTimer::EventLoopTimer;
use bun_install::git_runner::GitSubprocess;
use bun_install::{LifecycleScriptSubprocess, SecurityScanSubprocess};
use bun_io::{EventLoopCtx, IoEvent, Owner, PollTag};
use bun_ptr::RefPtr;
use bun_yarr::{Compiled, Text};

#[unsafe(no_mangle)]
fn __bun_remove_timer(_timer: &EventLoopTimer) {
    unreachable!("an EventLoopTimer is armed in a JavaScript VM's timer heap, and there is no VM here")
}

#[unsafe(no_mangle)]
fn __bun_js_event_loop_current() -> *mut () {
    unreachable!("a jsc::EventLoop is a JavaScript VM's, and there is no VM here")
}

#[unsafe(no_mangle)]
fn __bun_get_vm_ctx() -> EventLoopCtx {
    unreachable!("the JavaScript thread's EventLoopCtx is a VM's, and there is no VM here")
}

const NO_VM: &str = "a JavaScript VM's event loop, and there is no VM here";

/// The loops of a JavaScript VM, which this build never makes: the owners of the `link_impl_*!` arms below.
struct JsLoop;
struct SpawnSyncLoop;
struct JscLoop;

bun_io::link_impl_EventLoopCtx! {
    Js for JsLoop => |_this| {
        loop_() => unreachable!("{NO_VM}"),
        file_polls_ptr() => unreachable!("{NO_VM}"),
        increment_pending_unref_counter() => unreachable!("{NO_VM}"),
        after_event_loop_callback() => unreachable!("{NO_VM}"),
        set_after_event_loop_callback(_cb, _ctx) => unreachable!("{NO_VM}"),
        pipe_read_scratch() => unreachable!("{NO_VM}"),
    }
}

bun_io::link_impl_EventLoopCtx! {
    SpawnSync for SpawnSyncLoop => |_this| {
        loop_() => unreachable!("{NO_VM}"),
        file_polls_ptr() => unreachable!("{NO_VM}"),
        increment_pending_unref_counter() => unreachable!("{NO_VM}"),
        after_event_loop_callback() => unreachable!("{NO_VM}"),
        set_after_event_loop_callback(_cb, _ctx) => unreachable!("{NO_VM}"),
        pipe_read_scratch() => unreachable!("{NO_VM}"),
    }
}

bun_event_loop::link_impl_JsEventLoop! {
    Jsc for JscLoop => |_this| {
        iteration_number() => unreachable!("{NO_VM}"),
        uws_loop() => unreachable!("{NO_VM}"),
        event_loop_ctx() => unreachable!("{NO_VM}"),
        tick() => unreachable!("{NO_VM}"),
        auto_tick() => unreachable!("{NO_VM}"),
        auto_tick_active() => unreachable!("{NO_VM}"),
        global_object() => unreachable!("{NO_VM}"),
        bun_vm() => unreachable!("{NO_VM}"),
        enter() => unreachable!("{NO_VM}"),
        exit() => unreachable!("{NO_VM}"),
        enqueue_task(_task) => unreachable!("{NO_VM}"),
        enqueue_task_after_yield(_task) => unreachable!("{NO_VM}"),
        js_poster() => unreachable!("{NO_VM}"),
        env() => unreachable!("{NO_VM}"),
    }
}

/// The exit handlers of the processes that only `bun_runtime` spawns, each owned by a type of its name.
/// `bun_install` has its own (`LifecycleScript`, `InstallGit`, `SecurityScan`), and `bun_spawn` has `SyncWindows`.
mod runtime_processes {
    macro_rules! runtime_process_exit {
        ($($variant:ident)*) => {
            $(
                pub(super) struct $variant;
                bun_spawn::link_impl_ProcessExit! {
                    $variant for $variant => |_this| {
                        on_process_exit(_status, _rusage) => unreachable!(
                            concat!("a ", stringify!($variant), " process exited, and only bun_runtime spawns those")
                        ),
                    }
                }
            )*
        };
    }
    runtime_process_exit!(Subprocess Shell FilterRunHandle MultiRunHandle TestParallelWorker Cron ChromeProcess HostProcess);
}

/// `bun_runtime::dispatch::__bun_run_io`, with the arms of the owners that the package manager makes; registered
/// with `bun_runtime_js::dispatch`.
///
/// # Safety
/// `owner.ptr()` is the live value that the arm of `owner.tag()` casts it to.
pub(crate) unsafe fn run_io(owner: Owner, event: IoEvent) {
    match owner.tag() {
        PollTag::Process => {
            let process = owner.ptr().cast::<bun_spawn::process::Process>();
            let mut rusage = bun_spawn::process::rusage_zeroed();
            // SAFETY: a process frees its poll before it is freed, and `wait_posix` releases no ref.
            unsafe {
                if let Some(exit) = (*process).wait_posix(false, &mut rusage) {
                    exit.dispatch(&rusage);
                } else if let Some(exit) = RefPtr::init_ref(process).rewatch_or_reap(&mut rusage) {
                    exit.dispatch(&rusage);
                }
            }
        }
        // SAFETY: the contract of `__bun_run_io`.
        PollTag::LifecycleScript => unsafe {
            LifecycleScriptSubprocess::on_io(owner.ptr().cast(), owner.kind(), event)
        },
        PollTag::InstallGit => {
            // SAFETY: the contract of `__bun_run_io`; it frees itself once it has read to the end.
            let this = unsafe { RefPtr::init_ref(owner.ptr().cast::<GitSubprocess>()) };
            this.on_io(owner.kind(), event);
        }
        PollTag::SecurityScanReader => {
            // SAFETY: the contract of `__bun_run_io`.
            unsafe { &mut *owner.ptr().cast::<SecurityScanSubprocess<'_>>() }.on_io(event)
        }
        PollTag::SecurityScanWriter => {
            // SAFETY: the contract of `__bun_run_io`.
            unsafe { &mut *owner.ptr().cast::<SecurityScanSubprocess<'_>>() }.on_writer_io(event)
        }
        PollTag::Null => debug_assert!(false, "a poll with no owner fired"),
        tag => unreachable!(
            "a poll of PollTag {} fired, and only bun_runtime makes those",
            tag as u8
        ),
    }
}

#[unsafe(no_mangle)]
fn __bun_dns_register_quic(_pc: bun_http::H3::PendingConnect) {
    unreachable!("HTTP/3 needs lsquic, which this build does not link")
}

/// The text of `string` as JavaScriptCore has it: `toWTFString()` of RegularExpression.cpp.
fn with_text<R>(string: &BunString, f: impl FnOnce(Text<'_>) -> R) -> R {
    if string.is_utf16() {
        return f(Text::Utf16(string.utf16()));
    }
    if !string.is_utf8() {
        return f(Text::Latin1(string.latin1()));
    }
    let bytes = string.byte_slice();
    if bytes.is_ascii() {
        return f(Text::Latin1(bytes));
    }
    let units: Vec<u16> = String::from_utf8_lossy(bytes).encode_utf16().collect();
    f(Text::Utf16(&units))
}

/// `bun_jsc::regular_expression::__bun_regex_compile`: the pattern with no flags, on `bun_yarr`.
#[unsafe(no_mangle)]
fn __bun_regex_compile(pattern: &BunString) -> Option<NonNull<()>> {
    let compiled = with_text(pattern, |text| Compiled::new(text, 0)).ok()?;
    Some(NonNull::from(Box::leak(Box::new(compiled))).cast())
}

/// # Safety
/// `regex` is from `__bun_regex_compile` and not dropped yet.
#[unsafe(no_mangle)]
unsafe fn __bun_regex_matches(regex: NonNull<()>, input: &BunString) -> bool {
    // SAFETY: the contract of this function.
    let compiled = unsafe { regex.cast::<Compiled>().as_ref() };
    let mut offsets = vec![0u32; compiled.offsets_len()];
    with_text(input, |text| compiled.exec(text, 0, &mut offsets))
}

/// # Safety
/// `regex` is from `__bun_regex_compile`, and is not used after this.
#[unsafe(no_mangle)]
unsafe fn __bun_regex_drop(regex: NonNull<()>) {
    // SAFETY: the contract of this function.
    drop(unsafe { Box::from_raw(regex.cast::<Compiled>().as_ptr()) });
}

#[cfg(test)]
mod tests {
    use bun_install_types::NodeLinker::{Behavior, PnpmMatcher, create_matcher};

    fn matcher(patterns: &[&[u8]], behavior: Behavior) -> PnpmMatcher {
        PnpmMatcher {
            matchers: patterns.iter().map(|it| create_matcher(it)).collect(),
            behavior,
        }
    }

    /// `publicHoistPattern` and friends: package-name globs, compiled to anchored patterns and matched on `bun_yarr`.
    #[test]
    fn package_name_patterns_match_on_yarr() {
        let types = matcher(&[b"@types/*", b"eslint"], Behavior::AllMatchersInclude);
        assert!(types.is_match(b"@types/node"));
        assert!(types.is_match(b"eslint"));
        assert!(!types.is_match(b"eslint-plugin-react"));
        assert!(!types.is_match(b"x@types/node"));
        assert!(!types.is_match(b"react"));

        let mixed = matcher(&[b"*", b"!react*"], Behavior::HasExcludeAndIncludeMatchers);
        assert!(mixed.is_match(b"vue"));
        assert!(!mixed.is_match(b"react-dom"));

        let excluded = matcher(&[b"!a.b"], Behavior::AllMatchersExclude);
        assert!(!excluded.is_match(b"a.b"));
        assert!(excluded.is_match(b"axb"), "the dot is escaped");
    }
}
