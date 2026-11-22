//! `tabkit-ops <command> [args]`: runs one platform command in the repository of the caller.

use std::process::ExitCode;

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
    if name == "lib" {
        return match materialize(&cache_root()) {
            Ok(dir) => {
                println!("{}", dir.join("lib").display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("✗ cannot write the shell library to the cache: {e}");
                ExitCode::from(1)
            }
        };
    }
    eprintln!("✗ no command `{name}`. `tabkit-ops list` prints the commands");
    ExitCode::from(2)
}
