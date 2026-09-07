//! The standalone runner: the same forms the library serves, from a shell. The form comes from
//! `--file form.toml` or from field flags; the answers leave as TOML on stdout (the form itself
//! draws on stderr), so `terminal_choice … > answers.toml` composes. Exit codes: 0 answered, 1 cancelled,
//! 2 the form or terminal was unusable.

use terminal_choice::{run, Form, Outcome};

const USAGE: &str = "\
terminal_choice — fill a form in the terminal, get the answers as TOML on stdout

  terminal_choice --file FORM.toml [--run-checks]
  terminal_choice [--title T] [--comment TEXT] [--checkbox SPEC] [--radio SPEC] [--text SPEC] …

--run-checks answers a file's check_if / enabled_if predicates by running each as a shell
command: exit 0 means yes. Off by default — a definition file is someone's shell to run.

Field flags build the form in the order given. SPEC grammars:
  --checkbox \"Label: opt, opt, …\"     pre-check with a trailing   = opt, opt
  --radio    \"Label: opt, opt, …\"     pre-choose with a trailing  = opt
  --text     \"Label\"                  pre-fill with               = value

Keys: ↑/↓ move · space picks · enter next/submit · esc cancels (exit 1, no output)";

/// Whether `check` succeeds as a shell command. Output is discarded — a predicate answers with
/// its status, and an installer-detection one-liner's chatter is not this form's business.
fn shell_says_yes(check: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(check)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let built = match args.as_slice() {
        [] | [_] if matches!(args.first().map(String::as_str), None | Some("-h" | "--help")) => {
            println!("{USAGE}");
            return;
        }
        // A file IS the whole definition — mixing it with field flags would raise ordering
        // questions ("before or after the file's items?") that nothing needs answered.
        [flag, path] if flag == "--file" => std::fs::read_to_string(path)
            .map_err(|err| format!("{path}: {err}"))
            .and_then(|text| Form::from_toml(&text)),
        // The file's `check_if` / `enabled_if` predicates, answered THIS binary's way: run each
        // as a shell command and read its exit status. That is one program's choice of what a
        // predicate means, not the library's — which knows nothing about shells.
        [flag, path, run] | [flag, run, path] if flag == "--file" && run == "--run-checks" => {
            std::fs::read_to_string(path)
                .map_err(|err| format!("{path}: {err}"))
                .and_then(|text| Form::from_toml_with(&text, shell_says_yes))
        }
        [flag, ..] if flag == "--file" => {
            Err("--file takes one path, optionally with --run-checks, and no other flags".into())
        }
        _ => Form::from_args(args),
    };
    let mut form = match built {
        Ok(form) => form,
        Err(why) => {
            eprintln!("terminal_choice: {why}");
            std::process::exit(2);
        }
    };
    match run(&mut form) {
        Ok(Outcome::Submitted) => print!("{}", form.answers_toml()),
        Ok(Outcome::Cancelled) => {
            eprintln!("terminal_choice: cancelled — no answers");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("terminal_choice: {err}");
            std::process::exit(2);
        }
    }
}
