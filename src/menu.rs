//! A numbered menu — the simplest thing here: a question, a list, and a number typed in answer.
//!
//! Not a [`Form`](crate::Form). A form is a cursor over rows and every row is an answer; a menu is
//! ONE answer, chosen by its number, and a cursor would be a detour on the way to typing it. So
//! there is no focus list and nothing to walk: digits accumulate, Enter picks, Esc leaves. What it
//! shares with the form is everything underneath — the same raw mode, the same single-write
//! repaint, and the same shape: `render` and `apply` are pure and tested without a terminal, and
//! [`run_menu`] is the few lines that connect them to one.
//!
//! An option can be LOCKED: shown, dimmed, and refused with a reason when its number is typed.
//! That is how a menu says what will be on offer without pretending it already is.

use console::{Key, Term};

use crate::ui::{_await_input, _input_fd, _paint, RawMode};

/// One line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub label: String,
    /// `None` may be picked. `Some(why)` is shown dimmed, and typing its number is refused with
    /// `why` — the option exists, and this is what stands in its way.
    pub locked: Option<String>,
}

/// A question with numbered answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menu {
    /// Lines above the question — what was detected, what is on offer.
    pub intro: Vec<String>,
    pub question: String,
    pub picks: Vec<Pick>,
    /// The digits typed so far.
    pub typed: String,
    /// Why the last Enter was refused. Shown until the next key, then gone.
    pub refused: Option<String>,
}

/// How a menu ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    /// The index into [`Menu::picks`] — not the number typed, which is one higher.
    Picked(usize),
    Cancelled,
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Redraw,
    Ignored,
    Done(Chosen),
}

impl Menu {
    #[must_use]
    pub fn new(question: impl Into<String>) -> Self {
        Self { question: question.into(), ..Self::default() }
    }

    /// A line above the question. Several calls, several lines.
    #[must_use]
    pub fn intro(mut self, line: impl Into<String>) -> Self {
        self.intro.push(line.into());
        self
    }

    /// An option that may be picked.
    #[must_use]
    pub fn pick(mut self, label: impl Into<String>) -> Self {
        self.picks.push(Pick { label: label.into(), locked: None });
        self
    }

    /// An option that is shown and refused, with the reason — see [`Pick::locked`].
    #[must_use]
    pub fn locked(mut self, label: impl Into<String>, why: impl Into<String>) -> Self {
        self.picks.push(Pick { label: label.into(), locked: Some(why.into()) });
        self
    }

    /// What Enter would do with what has been typed: the index picked, or why not.
    ///
    /// Numbers are one-based on screen and the answer is zero-based, because the screen is for a
    /// person and the index is for `picks[]` — and a menu that showed `0)` would look broken.
    pub fn answer(&self) -> Result<usize, String> {
        let shown = self.picks.len();
        let Ok(number) = self.typed.parse::<usize>() else {
            return Err("type a number".to_string());
        };
        match number.checked_sub(1).and_then(|at| self.picks.get(at).map(|pick| (at, pick))) {
            None => Err(format!("there is no {number} — pick 1 to {shown}")),
            Some((_, Pick { locked: Some(why), .. })) => Err(format!("{number} is not on offer yet — {why}")),
            Some((at, _)) => Ok(at),
        }
    }

    /// How many digits the longest number needs — what aligns the list and caps the typing.
    fn digits(&self) -> usize {
        self.picks.len().max(1).to_string().len()
    }
}

/// The menu as displayable lines, clipped to `width` so no line can wrap — the same rule the form
/// lives by, for the same reason: the repaint climbs back over exactly as many lines as it drew.
pub(crate) fn render(menu: &Menu, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = menu.intro.clone();
    lines.push(menu.question.clone());
    let digits = menu.digits();
    for (at, pick) in menu.picks.iter().enumerate() {
        let number = format!("{:>digits$})", at + 1);
        lines.push(match &pick.locked {
            // Dim, like a disabled box: shown so the reader knows it exists, and quiet about it.
            Some(why) => console::style(format!("  {number} {}  — not yet: {why}", pick.label)).dim().to_string(),
            None => format!("  {number} {}", pick.label),
        });
    }
    // The prompt carries a caret, as the form's text field does, so an empty line still reads
    // as "type here". Nothing is inverted: there is no cursor to mark, only a place to type.
    lines.push(format!("> {}▏", menu.typed));
    if let Some(why) = &menu.refused {
        lines.push(console::style(format!("  ✗ {why}")).red().bold().to_string());
    }
    lines.push(console::style("type a number · enter picks · esc cancels").dim().to_string());
    lines.into_iter().map(|line| console::truncate_str(&line, width, "…").into_owned()).collect()
}

/// The whole keyboard contract:
/// - Digits accumulate, up to as many as the longest number needs — a fifth digit on a four-item
///   menu is dropped rather than queued into an answer that cannot be right.
/// - Backspace erases one.
/// - Enter picks what was typed, or explains the refusal and clears the digits to try again.
///   With nothing typed it does nothing at all.
/// - Esc cancels. Anything else is ignored.
pub(crate) fn apply(menu: &mut Menu, key: Key) -> MenuAction {
    // Any key at all withdraws the last refusal: the message answered the Enter that caused it.
    let had_refused = menu.refused.take().is_some();
    match key {
        Key::Escape => MenuAction::Done(Chosen::Cancelled),
        Key::Char(digit) if digit.is_ascii_digit() => {
            if menu.typed.len() >= menu.digits() {
                return if had_refused { MenuAction::Redraw } else { MenuAction::Ignored };
            }
            menu.typed.push(digit);
            MenuAction::Redraw
        }
        Key::Backspace => match menu.typed.pop() {
            Some(_) => MenuAction::Redraw,
            None if had_refused => MenuAction::Redraw,
            None => MenuAction::Ignored,
        },
        Key::Enter if menu.typed.is_empty() => {
            if had_refused { MenuAction::Redraw } else { MenuAction::Ignored }
        }
        Key::Enter => match menu.answer() {
            Ok(at) => MenuAction::Done(Chosen::Picked(at)),
            Err(why) => {
                menu.refused = Some(why);
                menu.typed.clear();
                MenuAction::Redraw
            }
        },
        _ if had_refused => MenuAction::Redraw,
        _ => MenuAction::Ignored,
    }
}

/// Show `menu` and return what was chosen. Draws on stderr, as the form does, so stdout stays
/// clean for whatever the caller prints about the answer.
pub fn run_menu(menu: &mut Menu) -> std::io::Result<Chosen> {
    let term = Term::buffered_stderr();
    if !term.is_term() {
        return Err(std::io::Error::other("interactive menus need a terminal (stderr is not one)"));
    }
    let (fd, _tty_handle) = _input_fd()?;
    let _raw = RawMode::engage(fd)?;
    term.hide_cursor()?;
    term.flush()?;
    let mut on_screen = 0;
    let chosen = loop {
        let width = (term.size().1 as usize).max(20);
        let lines = render(menu, width);
        _paint(&term, &lines, on_screen)?;
        on_screen = lines.len();
        let action = loop {
            match _await_input(fd) {
                Ok(true) => {}
                Ok(false) | Err(_) => break MenuAction::Done(Chosen::Cancelled),
            }
            match term.read_key() {
                Err(_) => break MenuAction::Done(Chosen::Cancelled),
                Ok(key) => match apply(menu, key) {
                    MenuAction::Ignored => continue,
                    other => break other,
                },
            }
        };
        if let MenuAction::Done(chosen) = action {
            break chosen;
        }
    };
    // Off the screen the same way the form goes: one write, nothing left behind.
    _paint(&term, &[], on_screen)?;
    term.show_cursor()?;
    term.flush()?;
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu::new("Which do you want?")
            .intro("Detected: a machine.")
            .pick("packages")
            .locked("configs", "a config owns a file, and files are not modelled")
            .pick("phone")
    }

    fn plain(menu: &Menu) -> Vec<String> {
        render(menu, 120).iter().map(|line| console::strip_ansi_codes(line).trim_end().to_string()).collect()
    }

    /// The whole picture: intro, question, numbered options with the locked one saying why, a
    /// prompt with a caret, and the legend. Numbers are one-based on screen.
    #[test]
    fn a_menu_draws_its_options_numbered_from_one_with_locked_ones_explained() {
        assert_eq!(
            plain(&menu()),
            [
                "Detected: a machine.",
                "Which do you want?",
                "  1) packages",
                "  2) configs  — not yet: a config owns a file, and files are not modelled",
                "  3) phone",
                "> ▏",
                "type a number · enter picks · esc cancels",
            ]
        );
        // The locked line is dim, as a disabled box is; the others are plain.
        let lines = render(&menu(), 120);
        assert_eq!(
            lines[3],
            console::style("  2) configs  — not yet: a config owns a file, and files are not modelled").dim().to_string()
        );
        assert_eq!(lines[2], "  1) packages");
    }

    /// Digits show as they are typed, Enter picks the option that number names — zero-based on
    /// the way out, because the index is for `picks[]` and the number was for the person.
    #[test]
    fn typing_a_number_and_enter_picks_that_option() {
        let mut menu = menu();
        assert_eq!(apply(&mut menu, Key::Char('3')), MenuAction::Redraw);
        assert_eq!(plain(&menu)[5], "> 3▏");
        assert_eq!(apply(&mut menu, Key::Enter), MenuAction::Done(Chosen::Picked(2)));
    }

    /// A locked option and a number that names nothing are both refused, with a reason under the
    /// prompt, and the digits cleared so the next attempt starts fresh. The next key withdraws
    /// the message. Enter with nothing typed does nothing.
    #[test]
    fn refusals_explain_themselves_and_clear_on_the_next_key() {
        let mut menu = menu();
        assert_eq!(apply(&mut menu, Key::Enter), MenuAction::Ignored, "nothing typed, nothing said");

        apply(&mut menu, Key::Char('2'));
        assert_eq!(apply(&mut menu, Key::Enter), MenuAction::Redraw);
        assert_eq!(
            menu.refused.as_deref(),
            Some("2 is not on offer yet — a config owns a file, and files are not modelled")
        );
        assert!(menu.typed.is_empty(), "cleared for another go");
        assert!(plain(&menu).iter().any(|line| line.trim_start().starts_with("✗ 2 is not on offer yet")), "{:#?}", plain(&menu));

        apply(&mut menu, Key::Char('9'));
        assert_eq!(menu.refused, None, "the next key withdraws the message");
        apply(&mut menu, Key::Enter);
        assert_eq!(menu.refused.as_deref(), Some("there is no 9 — pick 1 to 3"));

        // A non-digit while a refusal shows still repaints — the message has to go — and is
        // otherwise ignored, as it always was.
        assert_eq!(apply(&mut menu, Key::Char('x')), MenuAction::Redraw);
        assert_eq!(apply(&mut menu, Key::Char('x')), MenuAction::Ignored);
    }

    /// Backspace edits, Esc leaves, and the digits are capped at what the longest number needs.
    #[test]
    fn backspace_edits_escape_cancels_and_digits_are_capped() {
        let mut menu = menu();
        apply(&mut menu, Key::Char('1'));
        assert_eq!(apply(&mut menu, Key::Char('2')), MenuAction::Ignored, "three options need one digit");
        assert_eq!(menu.typed, "1");
        assert_eq!(apply(&mut menu, Key::Backspace), MenuAction::Redraw);
        assert_eq!(apply(&mut menu, Key::Backspace), MenuAction::Ignored, "nothing left to erase");
        assert_eq!(apply(&mut menu, Key::Escape), MenuAction::Done(Chosen::Cancelled));

        let mut wide = Menu::new("?");
        for at in 0..12 {
            wide = wide.pick(format!("option {at}"));
        }
        apply(&mut wide, Key::Char('1'));
        apply(&mut wide, Key::Char('2'));
        assert_eq!(wide.typed, "12", "twelve options take two digits");
        assert_eq!(apply(&mut wide, Key::Enter), MenuAction::Done(Chosen::Picked(11)));
        assert!(plain(&wide).iter().any(|line| line.starts_with("   1) option 0")), "numbers right-aligned to two digits");
    }

    /// No line may exceed the width it was drawn for — a wrapped line breaks the repaint, exactly
    /// as it would for the form.
    #[test]
    fn lines_are_clipped_to_the_width() {
        let long = Menu::new("?").locked("thing", "a reason long enough to run well past any terminal that was ever made, twice over");
        for line in render(&long, 30) {
            assert!(console::measure_text_width(&line) <= 30, "{line:?}");
        }
    }
}
