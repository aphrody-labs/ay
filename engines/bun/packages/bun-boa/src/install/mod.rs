//! `bun install` (`i`, `ci`), `bun add` (`a`), `bun remove` (`rm`, `r`, `uninstall`): Bun's package manager
//! (`bun_install`) driven the way src/runtime/cli/install_command.rs, add_command.rs and remove_command.rs drive it,
//! with the `ContextData` that `Command::create_context_data` gives these commands (they parse their own flags).
//! Behind the `install` feature: what it reaches of uSockets, BoringSSL and Bun's C++ is still a stand-in (native.rs).

mod digests;
mod hooks;
mod native;
mod syscalls;
mod uws_dispatch;
mod wtf;

// The C ABI that nothing names in Rust: `posix_spawn_bun` and spawnSync's signal forwarding.
use bun_spawn_rs as _;

use bun_core::{Global, Output};
use bun_install::package_manager_real::{
    CommandLineArguments, PackageManager, Subcommand, install_with_manager,
    update_package_json_and_install_and_cli, update_package_json_and_install_with_manager,
};
use bun_options_types::context::ContextData;

/// The subcommand of `bun <command>`, as `Command::which` maps it, or `None` if this is not one of them.
pub fn subcommand(command: &str, args: &[String]) -> Option<Subcommand> {
    Some(match command {
        "i" | "install" if args.iter().any(|arg| arg == "-g" || arg == "--global") => {
            Subcommand::Add
        }
        "i" | "install" | "ci" => Subcommand::Install,
        "add" | "a" => Subcommand::Add,
        "r" | "remove" | "rm" | "uninstall" => Subcommand::Remove,
        _ => return None,
    })
}

pub fn main(subcommand: Subcommand) -> ! {
    // As src/runtime/bin_entry does: a write to a closed pipe or past the file size limit is an error, not a signal.
    // SAFETY: setting signal dispositions to SIG_IGN, before any other thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
    }
    // As `Cli::start`: the package manager wakes the main thread's loop, which it finds by this.
    bun_crash_handler::cli_state::set_main_thread_id(bun_threading::current_thread_id());
    uws_dispatch::register();
    bun_runtime_js::dispatch::register_run_io(hooks::run_io);
    bun_core::set_start_time(bun_core::time::nano_timestamp());
    bun_core::output::stdio::init();
    let _flush = bun_core::output::flush_guard();
    let log_ptr: *mut bun_ast::Log = Box::leak(Box::new(bun_ast::Log::init()));
    let ctx: &'static mut ContextData = Box::leak(Box::new(ContextData::default()));
    ctx.log = log_ptr;
    ctx.start_time = bun_core::start_time();
    // SAFETY: `ctx` is leaked, so it outlives the process.
    unsafe { bun_options_types::context::set_global(ctx) };

    let result = match subcommand {
        Subcommand::Install => install(ctx),
        _ => update_package_json_and_install(ctx, subcommand),
    };
    match result {
        // `Global::exit` runs what the package manager queued for exit, as after `Cli::start` in bin_entry.
        Ok(()) => Global::exit(0),
        Err(bun_install::Error::InstallFailed | bun_install::Error::InvalidPackageJSON) => {
            // SAFETY: `log_ptr` is the leaked `Log` above; the package manager is done with it.
            let _ = unsafe { &*log_ptr }.print(Output::error_writer());
            Global::exit(1)
        }
        Err(err) => {
            // SAFETY: as above.
            let _ = unsafe { &*log_ptr }.print(Output::error_writer());
            bun_crash_handler::handle_root_error(err)
        }
    }
}

fn install(ctx: &mut ContextData) -> Result<(), bun_install::Error> {
    let cli = CommandLineArguments::parse(Subcommand::Install)?;
    refuse_analyze(&cli);
    let subcommand = if cli.positionals.len() > 1 {
        Subcommand::Add
    } else {
        Subcommand::Install
    };

    let (manager, original_cwd) = PackageManager::init(&mut *ctx, &cli, Subcommand::Install)?;

    if subcommand == Subcommand::Add {
        manager.subcommand = Subcommand::Add;
        if manager.options.should_print_command_name() {
            bun_core::prettyln!(
                "<r><b>bun add <r><d>v{}<r>\n",
                Global::package_json_version_with_sha,
            );
            Output::flush();
        }
        return update_package_json_and_install_with_manager(manager, &mut *ctx, &original_cwd);
    }

    if manager.options.should_print_command_name() {
        bun_core::prettyln!(
            "<r><b>bun install <r><d>v{}<r>\n",
            Global::package_json_version_with_sha,
        );
        Output::flush();
    }

    let root_package_json_path = manager.root_package_json_path;
    install_with_manager(manager, &mut *ctx, root_package_json_path, &original_cwd)?;

    if manager.any_failed_to_install {
        Global::exit(1);
    }
    Ok(())
}

fn update_package_json_and_install(
    ctx: &mut ContextData,
    subcommand: Subcommand,
) -> Result<(), bun_install::Error> {
    let cli = CommandLineArguments::parse(subcommand)?;
    refuse_analyze(&cli);
    update_package_json_and_install_and_cli(ctx, subcommand, &cli)
}

/// `--analyze` bundles the source files to find what they import (src/runtime/cli/pm_update_package_json.rs), and
/// the bundler is the `build` feature's.
fn refuse_analyze(cli: &CommandLineArguments) {
    if cli.analyze {
        bun_core::pretty_errorln!("<r><red>error<r>: this bun's package manager does not support --analyze yet");
        Global::exit(1);
    }
}
