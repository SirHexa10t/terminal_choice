//! Yes/no questions put to the user.
//!
//! The forms' one-question sibling: no navigation, no state — ask, read a line, decide.
//!
//! Three entry points, named for the prompt they print, because that suffix is the convention
//! every CLI uses to show which way an empty answer falls: `_prompt_Yn` → `[Y/n]`, `_prompt_yN` →
//! `[y/N]`, `_prompt_yn` → `[y/n]`, which insists on an explicit answer.

use std::io::{IsTerminal, Write};

/// Ask, defaulting to **yes** when the answer is just Enter.
#[allow(non_snake_case)] // The capital IS the documentation: it mirrors the `[Y/n]` it prints.
pub fn prompt_Yn(question: &str) -> bool {
    _ask(question, Some(true))
}

/// Ask, defaulting to **no** when the answer is just Enter. The right one for anything
/// destructive: a reflexive Enter should leave the world alone.
#[allow(non_snake_case)] // As above — `_prompt_yN` prints `[y/N]`, and the case says which.
pub fn prompt_yN(question: &str) -> bool {
    _ask(question, Some(false))
}

/// Ask with **no default**: Enter re-asks, and only `y` or `n` ends it. For a choice where
/// guessing on the user's behalf would be presumptuous.
pub fn prompt_yn(question: &str) -> bool {
    _ask(question, None)
}

/// The one implementation. `default` is what an empty line means, or `None` to re-ask until the
/// answer is unambiguous.
///
/// Written to stderr so a prompt never lands in a redirected stdout, and read from stdin, which is
/// what the person is typing into.
///
/// With no terminal on stdin there is nobody to ask, so the default is taken without printing
/// anything — and `_prompt_yn`, having none to take, answers no. A caller doing something
/// destructive should not lean on that: check [`std::io::IsTerminal`] first and refuse outright,
/// rather than let a pipe silently decide.
fn _ask(question: &str, default: Option<bool>) -> bool {
    let fallback = default.unwrap_or(false);
    if !std::io::stdin().is_terminal() {
        return fallback;
    }
    let suffix = match default {
        Some(true) => "[Y/n]",
        Some(false) => "[y/N]",
        None => "[y/n]",
    };
    loop {
        eprint!("{question} {suffix} ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() || answer.is_empty() {
            return fallback; // read error, or EOF — the terminal went away mid-question
        }
        match _read_answer(&answer) {
            Some(verdict) => return verdict,
            // Empty with a default takes it; empty without one, or anything unrecognised, asks
            // again rather than guessing at what was meant.
            None if answer.trim().is_empty() => {
                if let Some(verdict) = default {
                    return verdict;
                }
            }
            None => {}
        }
    }
}

/// Pure: what one typed line means. `None` for "not an answer" — including an empty line, whose
/// meaning belongs to the caller's default rather than to the text.
fn _read_answer(line: &str) -> Option<bool> {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unambiguous_word_is_an_answer() {
        for yes in ["y", "Y", "yes", "YES", " yes ", "Yes\n"] {
            assert_eq!(_read_answer(yes), Some(true), "{yes:?}");
        }
        for no in ["n", "N", "no", "NO", " no ", "No\n"] {
            assert_eq!(_read_answer(no), Some(false), "{no:?}");
        }
        // Not answers: the empty line belongs to the default, and a near-miss must not be
        // guessed at — `yolo` starting with `y` is exactly the kind of thing that should re-ask
        // rather than be read as consent.
        for neither in ["", "\n", "  ", "yolo", "nope", "maybe", "1", "sure"] {
            assert_eq!(_read_answer(neither), None, "{neither:?}");
        }
    }
}
