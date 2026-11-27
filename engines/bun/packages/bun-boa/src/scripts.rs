//! `bun run <script>`: a `package.json` script through `sh -c`, as `RunCommand` does it: `pre<name>` and
//! `post<name>` around it, `node_modules/.bin` of every directory up to the root ahead of `$PATH`, the
//! arguments after the name appended, quoted. `bun run` alone lists the scripts.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The nearest `package.json` from `cwd` up, with its directory.
fn package_json(cwd: &Path) -> Option<(PathBuf, serde_json::Value)> {
    cwd.ancestors().find_map(|dir| {
        let text = bun_sys::File::read_from(
            bun_sys::Fd::cwd(),
            dir.join("package.json").as_os_str().as_encoded_bytes(),
        )
        .ok()?;
        Some((dir.to_path_buf(), serde_json::from_slice(&text).ok()?))
    })
}

fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b))
    {
        return arg.to_owned();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

fn path_with_bins(dir: &Path) -> std::ffi::OsString {
    let bins = dir.ancestors().map(|d| d.join("node_modules/.bin"));
    let rest = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    // The running `bun` first, so a script that calls `bun` gets this one.
    let own = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    std::env::join_paths(own.into_iter().chain(bins).chain(rest)).unwrap_or_default()
}

fn sh(name: &str, script: &str, dir: &Path, package: &serde_json::Value) -> i32 {
    eprintln!("\x1b[2m$\x1b[0m \x1b[2m{script}\x1b[0m");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("PATH", path_with_bins(dir));
    command
        .env("npm_lifecycle_event", name)
        .env("npm_lifecycle_script", script);
    if let Some(name) = package.get("name").and_then(|it| it.as_str()) {
        command.env("npm_package_name", name);
    }
    if let Some(version) = package.get("version").and_then(|it| it.as_str()) {
        command.env("npm_package_version", version);
    }
    if let Ok(exe) = std::env::current_exe() {
        command
            .env("npm_execpath", &exe)
            .env("npm_node_execpath", &exe);
    }
    command.env(
        "npm_config_user_agent",
        format!("bun/{}", bun_core::package_json_version),
    );
    match command.status() {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
        }
        Err(err) => {
            eprintln!("error: Could not run sh: {err}");
            1
        }
    }
}

/// Whether `bun run` takes `name` as a file path rather than a script name.
pub fn is_path(name: &str) -> bool {
    name == "."
        || name == ".."
        || name.starts_with("./")
        || name.starts_with("../")
        || name.starts_with('/')
}

/// `None` if there is no script named `name`.
pub fn run(name: &str, args: &[String]) -> Option<i32> {
    let cwd = std::env::current_dir().ok()?;
    let (dir, package) = package_json(&cwd)?;
    let scripts = package.get("scripts")?.as_object()?;
    let script = scripts.get(name)?.as_str()?;
    let lifecycle = |hook: String| {
        scripts
            .get(&hook)
            .and_then(|s| s.as_str())
            .map(|s| (hook.clone(), s.to_owned()))
    };
    if let Some((hook, pre)) = lifecycle(format!("pre{name}")) {
        let code = sh(&hook, &pre, &dir, &package);
        if code != 0 {
            return Some(code);
        }
    }
    let full = args.iter().fold(script.to_owned(), |mut all, arg| {
        all.push(' ');
        all.push_str(&quote(arg));
        all
    });
    let code = sh(name, &full, &dir, &package);
    if code != 0 {
        return Some(code);
    }
    if let Some((hook, post)) = lifecycle(format!("post{name}")) {
        return Some(sh(&hook, &post, &dir, &package));
    }
    Some(0)
}

/// `bun run` without a name: the scripts of the nearest `package.json`.
pub fn list() -> i32 {
    let found = std::env::current_dir()
        .ok()
        .and_then(|cwd| package_json(&cwd));
    let Some(scripts) = found
        .as_ref()
        .and_then(|(_, p)| p.get("scripts"))
        .and_then(|s| s.as_object())
    else {
        eprintln!("error: No package.json scripts found");
        return 1;
    };
    println!("scripts:");
    for (name, script) in scripts {
        println!(
            "\n bun run {name}\n   {}",
            script.as_str().unwrap_or_default()
        );
    }
    println!("\n{} scripts", scripts.len());
    0
}
