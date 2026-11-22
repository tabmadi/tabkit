//! `tabkit-ops <command> [args]`: runs one platform command in the repository of the caller.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use tabkit_ops::{cache_root, commands, materialize, native};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("usage: tabkit-ops <command> [args]. `tabkit-ops list` prints the commands");
        return ExitCode::from(2);
    };
    if name == "list" {
        for command in commands() {
            println!("{command}");
        }
        return ExitCode::SUCCESS;
    }
    if native::COMMANDS.contains(&name.as_str()) {
        let rest: Vec<String> = args.collect();
        return match native::run(&name, &rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("✗ {e:#}");
                ExitCode::from(1)
            }
        };
    }
    let dir = match materialize(&cache_root()) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("✗ cannot write the scripts to the cache: {e}");
            return ExitCode::from(1);
        }
    };
    if name == "lib" {
        println!("{}", dir.join("lib").display());
        return ExitCode::SUCCESS;
    }
    if !commands().contains(&name.as_str()) {
        eprintln!("✗ no command `{name}`. `tabkit-ops list` prints the commands");
        return ExitCode::from(2);
    }
    let error = Command::new("bash")
        .arg(dir.join(format!("{name}.sh")))
        .args(args)
        .exec();
    eprintln!("✗ cannot run bash: {error}");
    ExitCode::from(1)
}
