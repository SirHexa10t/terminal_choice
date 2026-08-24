//! The interactive part, built to be tested without a terminal: [`focusables`] maps a form to
//! its interactive rows, [`render`] draws the whole form as plain lines, and [`apply`] is the
//! entire key-handling truth — all pure. [`run`] is the only thing that touches a terminal, and
//! it is a dozen lines of loop around them.

use crate::{Form, Item};
use console::{Key, Term};

/// How a run ended: with the answers meant, or abandoned (whatever was half-filled stays in the
/// form, but the caller should treat it as noise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Submitted,
    Cancelled,
}

/// One interactive row — what the focus can rest on. Comments have no representation here,
/// which IS the "not selectable" guarantee: the focus list simply never contains them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// Option `option` of the choice group at `item` (checkbox or radio alike).
    Option { item: usize, option: usize },
    /// The text field at `item`.
    Text { item: usize },
    /// The final `[ Submit ]` row.
    Submit,
}

/// The focusable rows of `form`, in display order, always ending with Submit.
pub(crate) fn focusables(form: &Form) -> Vec<Focus> {
    let mut rows = Vec::new();
    for (index, item) in form.items.iter().enumerate() {
        match item {
            Item::Comment(_) => {}
            Item::Checkboxes { options, .. } | Item::Radio { options, .. } => {
                rows.extend((0..options.len()).map(|option| Focus::Option { item: index, option }));
            }
            Item::Text { .. } => rows.push(Focus::Text { item: index }),
        }
    }
    rows.push(Focus::Submit);
    rows
}

/// What a key did to the form.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// State or focus moved; repaint.
    Redraw,
    /// Nothing changed — and crucially, nothing is repainted. Terminals emit more than
    /// keystrokes (focus events, stray escape replies, mouse-tracking a previous program left
    /// enabled — a stream on every mouse MOVE); repainting per junk event turns an idle form
    /// into a render loop.
    Ignored,
    /// The Submit row was confirmed.
    Submit,
    /// Esc — leave everything as it stands and report [`Outcome::Cancelled`].
    Cancel,
}

/// The whole keyboard contract, in one testable place:
/// - ↑/↓ (and Tab) move focus over the interactive rows, wrapping; comments are never visited.
/// - Space toggles a checkbox or picks a radio — except in a text field, where it types.
/// - Printable keys type into the focused text field; Backspace erases.
/// - Enter confirms Submit, and elsewhere hops to the next row (fill, Enter, fill, Enter…).
/// - Ctrl+A ticks every checkbox in the form; when all are already ticked, it unticks them
///   all. One key for both directions, because a classic terminal sends the identical byte
///   for Ctrl+A and Ctrl+Shift+A — they cannot be told apart. (`console` maps that byte to
///   `Key::Home`, so the physical Home key does the same, harmlessly.) Radios and text fields
///   are untouched: "all" is only a meaningful answer for choose-many.
/// - Esc cancels.
pub(crate) fn apply(form: &mut Form, rows: &[Focus], focus: &mut usize, key: Key) -> Action {
    let editing_text = matches!(rows[*focus], Focus::Text { .. });
    match key {
        Key::Escape => return Action::Cancel,
        // Ctrl+A on a classic terminal (and the Home key). Toggle: all on, unless already all
        // on — then all off. Only checkbox groups; there is no "all" for radios or text.
        Key::Home => {
            let mut boxes = 0;
            let mut ticked = 0;
            for item in &form.items {
                if let Item::Checkboxes { checked, .. } = item {
                    boxes += checked.len();
                    ticked += checked.iter().filter(|on| **on).count();
                }
            }
            if boxes == 0 {
                return Action::Ignored;
            }
            let everything_on = ticked == boxes;
            for item in &mut form.items {
                if let Item::Checkboxes { checked, .. } = item {
                    checked.iter_mut().for_each(|on| *on = !everything_on);
                }
            }
        }
        Key::ArrowDown | Key::Tab => *focus = (*focus + 1) % rows.len(),
        Key::ArrowUp => *focus = (*focus + rows.len() - 1) % rows.len(),
        Key::Enter => match rows[*focus] {
            Focus::Submit => return Action::Submit,
            _ => *focus = (*focus + 1) % rows.len(),
        },
        Key::Char(' ') if !editing_text => {
            if let Focus::Option { item, option } = rows[*focus] {
                let mut mirror: Option<(String, bool)> = None;
                match &mut form.items[item] {
                    Item::Checkboxes { options, checked, .. } => {
                        checked[option] = !checked[option];
                        if form.mirror_duplicates {
                            mirror = Some((options[option].clone(), checked[option]));
                        }
                    }
                    // A radio picks; it does not un-pick — that's what makes it a radio.
                    Item::Radio { chosen, .. } => *chosen = Some(option),
                    _ => {}
                }
                // The same wording elsewhere IS the same entry: its boxes follow this one, in
                // every group at once — a thing cannot be both doomed and spared.
                if let Some((name, state)) = mirror {
                    for item in &mut form.items {
                        if let Item::Checkboxes { options, checked, .. } = item {
                            for (slot, text) in options.iter().enumerate() {
                                if *text == name {
                                    checked[slot] = state;
                                }
                            }
                        }
                    }
                }
            }
        }
        Key::Char(typed) => match rows[*focus] {
            Focus::Text { item } => {
                if let Item::Text { value, .. } = &mut form.items[item] {
                    value.push(typed);
                }
            }
            _ => return Action::Ignored, // a letter over a checkbox means nothing
        },
        Key::Backspace => match rows[*focus] {
            Focus::Text { item } => {
                if let Item::Text { value, .. } = &mut form.items[item] {
                    value.pop();
                }
            }
            _ => return Action::Ignored,
        },
        // Everything else a terminal can emit — unknown escape sequences, focus events, keys
        // this form has no meaning for. Changing nothing must also REPAINT nothing.
        _ => return Action::Ignored,
    }
    Action::Redraw
}

/// The input the form reads: stdin when it's a terminal, `/dev/tty` otherwise — the same
/// choice `console` makes internally, so the fd we wait on and configure is the fd it reads.
/// The `File` half keeps a non-stdin tty open for as long as the handle lives.
fn _input_fd() -> std::io::Result<(std::os::fd::RawFd, Option<std::fs::File>)> {
    use std::io::IsTerminal;
    use std::os::fd::AsRawFd;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        Ok((stdin.as_raw_fd(), None))
    } else {
        let tty = std::fs::File::open("/dev/tty")?;
        Ok((tty.as_raw_fd(), Some(tty)))
    }
}

/// Raw terminal mode for the WHOLE run, restored on drop.
///
/// `console` only enters raw mode inside each `read_key` call — correct for a one-off prompt,
/// but between this loop's keys the terminal would sit in cooked mode with echo on: every
/// arrow press would PRINT (`^[[B`) instead of acting, and canonical buffering would hold the
/// bytes back until Enter, so the form would seem deaf. (Exactly that shipped once — scripted
/// ptys masked it, because their input arrives pre-buffered with newlines in it.) Holding raw
/// for the run's lifetime gives byte-at-a-time reads with no echo; the output flags keep their
/// original state so `\n` still starts a fresh line.
struct RawMode {
    fd: std::os::fd::RawFd,
    original: libc::termios,
}

impl RawMode {
    fn engage(fd: std::os::fd::RawFd) -> std::io::Result<Self> {
        // SAFETY: tcgetattr/tcsetattr write only the termios handed to them; the fd is the
        // terminal this form runs on.
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut original) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            raw.c_oflag = original.c_oflag; // keep output post-processing: `\n` stays a newline
            if libc::tcsetattr(fd, libc::TCSADRAIN, &raw) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { fd, original })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the very state tcgetattr produced, on the same fd.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original);
        }
    }
}

/// Sleep until the terminal has input (or is gone). `Ok(true)`: a key is waiting, and
/// `Term::read_key` will return without ever reaching its zero-timeout polling. `Ok(false)`:
/// hangup — the terminal went away, which a caller should read as cancellation rather than
/// spin on (a hung-up fd stays "ready" forever without ever having input).
///
/// This is THE idle state of a running form, so it is a plain blocking `poll` owned here —
/// whether an idle form costs 0% CPU should not depend on a dependency's key-reading internals.
/// It only works because [`RawMode`] holds the terminal non-canonical: cooked mode releases
/// bytes to `poll` a full line at a time.
fn _await_input(fd: std::os::fd::RawFd) -> std::io::Result<bool> {
    let mut watch = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: `poll` reads and writes only the pollfd handed to it, which lives on this
        // stack frame; a negative timeout blocks until the fd has news.
        let ready = unsafe { libc::poll(&mut watch, 1, -1) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue; // a signal woke us early; the wait itself is still on
            }
            return Err(err);
        }
        if watch.revents & libc::POLLIN != 0 {
            return Ok(true);
        }
        if watch.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(false);
        }
    }
}

/// The full form as displayable lines, the focused row inverted, everything clipped to `width`
/// so no line can wrap (a wrapped line would break the redraw arithmetic — the loop clears
/// exactly as many lines as it printed).
pub(crate) fn render(form: &Form, rows: &[Focus], focus: usize, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(title) = &form.title {
        lines.push(console::style(title).bold().to_string());
        lines.push(String::new());
    }
    // Content may carry its own colours (a caller highlighting matches). Scrubbed here when
    // asked; otherwise kept — which obliges the focus style below to survive them: an embedded
    // reset (`\x1b[0m`) would end the reverse-video mid-row, so every reset re-arms it.
    let clean = |text: String| match form.scrub_colors {
        true => console::strip_ansi_codes(&text).into_owned(),
        false => text,
    };
    let mark = |line: String, here: Focus| match rows[focus] == here {
        true => format!("\x1b[7m▸ {}\x1b[0m", line.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
        false => format!("  {line}"),
    };
    // Aligned forms interleave comments with options into one table, so a comment gets the
    // same lead-in an option's `[x] ` occupies and the columns hold.
    let comment_indent = if form.aligned { "      " } else { "  " };
    for (index, item) in form.items.iter().enumerate() {
        match item {
            Item::Comment(text) => {
                for comment_line in text.lines() {
                    lines.push(
                        console::style(format!("{comment_indent}{}", clean(comment_line.to_string())))
                            .dim()
                            .to_string(),
                    );
                }
            }
            Item::Checkboxes { label, options, checked } => {
                // An anonymous group draws no heading — it exists to sit flush inside
                // surrounding comments (a tree of rows, some of them tickable).
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, name) in options.iter().enumerate() {
                    let box_mark = if checked[option] { "[x]" } else { "[ ]" };
                    lines.push(mark(
                        format!("{box_mark} {}", clean(name.clone())),
                        Focus::Option { item: index, option },
                    ));
                }
            }
            Item::Radio { label, options, chosen } => {
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, name) in options.iter().enumerate() {
                    let dot = if *chosen == Some(option) { "(•)" } else { "( )" };
                    lines.push(mark(
                        format!("{dot} {}", clean(name.clone())),
                        Focus::Option { item: index, option },
                    ));
                }
            }
            Item::Text { label, value } => {
                lines.push(mark(format!("{label}: {value}▏"), Focus::Text { item: index }));
            }
        }
    }
    lines.push(String::new());
    lines.push(mark("[ Submit ]".to_string(), Focus::Submit));
    lines.push(
        console::style("↑/↓ move · space picks · ctrl+a all/none · enter next/submit · esc cancels")
            .dim()
            .to_string(),
    );
    lines.into_iter().map(|line| console::truncate_str(&line, width, "…").into_owned()).collect()
}

/// Run `form` interactively on the terminal; the form's own state carries the answers.
///
/// Drawn on stderr, so a scripted caller can pipe stdout (where a binary prints the answers)
/// while the form still appears. Not a terminal → an error naming the problem, not a hang.
pub fn run(form: &mut Form) -> std::io::Result<Outcome> {
    let term = Term::stderr();
    if !term.is_term() {
        return Err(std::io::Error::other(
            "interactive forms need a terminal (stderr is not one)",
        ));
    }
    let rows = focusables(form);
    let mut focus = 0;
    let mut on_screen = 0;
    let (fd, _tty_handle) = _input_fd()?;
    // Raw for the whole run (drops — and restores — on every path out of this function).
    let _raw = RawMode::engage(fd)?;
    term.hide_cursor()?;
    let outcome = loop {
        // Repaint only when something changed — the inner loop below eats the junk events a
        // terminal produces (focus reports, stray escapes) without a single write.
        term.clear_last_lines(on_screen)?;
        let width = term.size().1 as usize;
        let lines = render(form, &rows, focus, width.max(20));
        for line in &lines {
            term.write_line(line)?;
        }
        on_screen = lines.len();
        let action = loop {
            match _await_input(fd) {
                Ok(true) => {}
                // Hangup, or an unreadable terminal: nobody is there to answer.
                Ok(false) | Err(_) => break Action::Cancel,
            }
            match term.read_key() {
                // The terminal went away mid-question (hangup, ctrl-d): treat as walking off.
                Err(_) => break Action::Cancel,
                Ok(key) => match apply(form, &rows, &mut focus, key) {
                    Action::Ignored => continue,
                    decided => break decided,
                },
            }
        };
        match action {
            Action::Submit => break Outcome::Submitted,
            Action::Cancel => break Outcome::Cancelled,
            _ => {}
        }
    };
    term.clear_last_lines(on_screen)?;
    term.show_cursor()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new()
            .comment("can't touch this")
            .checkboxes("Tops", &["a", "b"])
            .radio("Size", &["S", "M"])
            .text("Name", "")
    }

    #[test]
    fn comments_are_unreachable_and_submit_is_always_last() {
        let form = form();
        let rows = focusables(&form);
        // 2 checkbox options + 2 radio options + 1 text + submit — and nothing for the comment.
        assert_eq!(rows.len(), 6);
        assert_eq!(rows.last(), Some(&Focus::Submit));
        assert!(rows.iter().all(|row| !matches!(row, Focus::Option { item: 0, .. })));
    }

    #[test]
    fn navigation_wraps_and_enter_walks_the_form() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::ArrowUp);
        assert_eq!(rows[focus], Focus::Submit, "up from the top wraps to the end");
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(focus, 0, "and back around");
        apply(&mut form, &rows, &mut focus, Key::Enter);
        assert_eq!(focus, 1, "enter off Submit is 'next', for fill-enter-fill-enter flow");
    }

    #[test]
    fn space_toggles_checkboxes_and_radios_stay_exclusive() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 0; // first checkbox option
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(form.checked("Tops"), ["a"]);
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert!(form.checked("Tops").is_empty(), "a second space un-checks");

        focus = 2; // first radio option
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(form.chosen("Size"), Some("S"));
        focus = 3;
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(form.chosen("Size"), Some("M"), "picking M un-picks S — exclusivity");
    }

    #[test]
    fn typing_lands_in_the_text_field_spaces_included() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 4; // the text field
        for ch in "Ada L".chars() {
            apply(&mut form, &rows, &mut focus, Key::Char(ch));
        }
        assert_eq!(form.text_value("Name"), Some("Ada L"), "space TYPES here, never toggles");
        apply(&mut form, &rows, &mut focus, Key::Backspace);
        apply(&mut form, &rows, &mut focus, Key::Char('é'));
        assert_eq!(form.text_value("Name"), Some("Ada é"), "unicode in, unicode out");
    }

    /// The raw-mode regression, pinned at the OS level with a real pty pair. The bug: the form
    /// idled OUTSIDE `console`'s per-read raw window, so between keys the terminal sat cooked —
    /// arrows echoed as `^[[B` instead of acting, and canonical buffering held every byte back
    /// until Enter, leaving the form deaf. Scripted-input tests missed it because their bytes
    /// arrive pre-buffered with newlines. This asserts the mechanism directly: [`RawMode`] must
    /// clear ECHO and ICANON (and restore them on drop), and a newline-less byte must become
    /// pollable — which cooked mode refuses.
    #[test]
    fn raw_mode_disables_echo_and_line_buffering_and_restores_on_drop() {
        let (master, follower) = pty_pair();
        // SAFETY: reading termios state of a fd this test owns.
        let cooked = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(follower, &mut t), 0, "the pty must answer tcgetattr");
            t
        };
        assert!(cooked.c_lflag & libc::ECHO != 0, "a fresh pty echoes — that IS the hazard");
        assert!(cooked.c_lflag & libc::ICANON != 0, "and buffers lines — the other half of it");

        {
            let _raw = RawMode::engage(follower).expect("engaging raw mode");
            // SAFETY: as above.
            let raw = unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                libc::tcgetattr(follower, &mut t);
                t
            };
            assert_eq!(raw.c_lflag & libc::ECHO, 0, "no echo while the form runs");
            assert_eq!(raw.c_lflag & libc::ICANON, 0, "no line buffering while the form runs");

            // The deafness half: a byte with NO newline must wake a poll under raw mode.
            // SAFETY: writing one byte to the master side this test owns.
            unsafe {
                assert_eq!(libc::write(master, b" ".as_ptr().cast(), 1), 1);
            }
            let mut watch = libc::pollfd { fd: follower, events: libc::POLLIN, revents: 0 };
            // SAFETY: polling a fd this test owns, bounded timeout.
            let ready = unsafe { libc::poll(&mut watch, 1, 500) };
            assert_eq!(ready, 1, "a lone keypress must be readable immediately under raw mode");
            assert!(watch.revents & libc::POLLIN != 0);
            // Drain it so the restore below starts clean.
            let mut sink = [0u8; 8];
            // SAFETY: reading from a fd this test owns into a stack buffer.
            unsafe { libc::read(follower, sink.as_mut_ptr().cast(), sink.len()) };
        }

        // SAFETY: as above.
        let restored = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(follower, &mut t);
            t
        };
        assert!(restored.c_lflag & libc::ECHO != 0, "dropping the guard must restore echo");
        assert!(restored.c_lflag & libc::ICANON != 0, "and line buffering");
        // SAFETY: closing fds this test opened.
        unsafe {
            libc::close(master);
            libc::close(follower);
        }
    }

    /// A pty pair for the raw-mode tests — the follower side is a genuine terminal, which is the
    /// whole point: these behaviours don't exist on pipes.
    fn pty_pair() -> (std::os::fd::RawFd, std::os::fd::RawFd) {
        let (mut master, mut follower) = (0, 0);
        // SAFETY: openpty writes the two fds it creates; null window-size/termios take defaults.
        let ok = unsafe {
            libc::openpty(
                &mut master,
                &mut follower,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(ok, 0, "openpty must succeed for these tests to mean anything");
        (master, follower)
    }

    /// Ctrl+A (which a classic terminal delivers as the same byte console maps to Home) ticks
    /// every checkbox; when everything is already ticked, it unticks everything. Radios and
    /// text stay untouched, and a form with no checkboxes ignores it entirely.
    #[test]
    fn ctrl_a_toggles_all_checkboxes_and_only_checkboxes() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 4; // even from a TEXT field, the toggle is form-wide
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Home), Action::Redraw);
        assert_eq!(form.checked("Tops"), ["a", "b"], "everything ticked at once");
        assert_eq!(form.chosen("Size"), None, "radios are not an 'all' kind of question");
        apply(&mut form, &rows, &mut focus, Key::Home);
        assert!(form.checked("Tops").is_empty(), "all-on toggles to all-off");
        // Partially ticked → Ctrl+A completes the set rather than clearing it.
        let Item::Checkboxes { checked, .. } = &mut form.items[1] else { panic!() };
        checked[0] = true;
        apply(&mut form, &rows, &mut focus, Key::Home);
        assert_eq!(form.checked("Tops"), ["a", "b"], "partial → filled, not cleared");

        let mut no_boxes = Form::new().text("Name", "");
        let rows = focusables(&no_boxes);
        let mut focus = 0;
        assert_eq!(apply(&mut no_boxes, &rows, &mut focus, Key::Home), Action::Ignored);
    }

    /// Coloured option text survives by default — including THROUGH the focus highlight, which
    /// must re-arm its reverse-video after every embedded reset instead of dying at the first
    /// one. `scrub_colors` is the opt-out.
    #[test]
    fn coloured_options_survive_focus_and_scrub_on_request() {
        let glow = "kill \x1b[30;41mfirefox\x1b[0m now";
        let coloured = Form::new().checkboxes("Pick", &[glow]);
        let rows = focusables(&coloured);
        let lines = render(&coloured, &rows, 0, 200);
        let focused = lines.iter().find(|l| l.contains("firefox")).unwrap();
        assert!(focused.contains("\x1b[30;41m"), "the caller's colours are kept: {focused:?}");
        assert!(
            focused.contains("\x1b[0m\x1b[7m"),
            "every embedded reset re-arms the focus reverse: {focused:?}"
        );
        let scrubbed = Form::new().checkboxes("Pick", &[glow]).scrub_colors();
        let rows = focusables(&scrubbed);
        let lines = render(&scrubbed, &rows, 0, 200);
        let row = lines.iter().find(|l| l.contains("firefox")).unwrap();
        assert!(!row.contains("30;41"), "scrubbed means gone: {row:?}");
    }

    /// Junk input — unknown escapes, letters over checkboxes — must change nothing AND paint
    /// nothing. A terminal with mouse-tracking left on streams events at every pointer move;
    /// repainting per event is how a form ends up burning CPU like a game.
    #[test]
    fn junk_input_is_ignored_not_repainted() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 0; // a checkbox row
        let before = form.clone();
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char('q')), Action::Ignored);
        assert_eq!(
            apply(&mut form, &rows, &mut focus, Key::UnknownEscSeq(vec!['<', '3', '5'])),
            Action::Ignored,
            "mouse-tracking noise"
        );
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Backspace), Action::Ignored);
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::PageUp), Action::Ignored);
        assert_eq!(form, before, "ignored means untouched");
        assert_eq!(focus, 0, "and unmoved");
        // The same keys over a text field are real input, not junk.
        focus = 4;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char('q')), Action::Redraw);
    }

    /// With `mirror_duplicates`, one entry shown under two headings is ONE entry: ticking it
    /// anywhere ticks it everywhere, and unticking likewise — a thing cannot be both doomed
    /// and spared. Without the flag, same-worded options stay independent.
    #[test]
    fn mirrored_duplicates_tick_and_untick_together() {
        let mut form = Form::new()
            .checkboxes("Top CPU", &["proc-a", "proc-b"])
            .checkboxes("Top memory", &["proc-b", "proc-c"])
            .mirror_duplicates();
        let rows = focusables(&form);
        let mut focus = 1; // "proc-b" in the CPU group
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(form.checked("Top CPU"), ["proc-b"]);
        assert_eq!(form.checked("Top memory"), ["proc-b"], "its twin followed");
        focus = 2; // "proc-b" in the memory group
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert!(form.checked("Top CPU").is_empty(), "unticking from the OTHER side works too");
        assert!(form.checked("Top memory").is_empty());

        let mut plain = Form::new()
            .checkboxes("A", &["same"])
            .checkboxes("B", &["same"]);
        let rows = focusables(&plain);
        let mut focus = 0;
        apply(&mut plain, &rows, &mut focus, Key::Char(' '));
        assert!(plain.checked("B").is_empty(), "without the flag, no linkage");
    }

    /// Aligned mode: comments interleaved between options share the options' column start —
    /// the comment gains the width of the `[x] ` lead-in, so a tree drawn across both row
    /// kinds keeps its columns.
    #[test]
    fn aligned_comments_share_the_options_column() {
        let anonymous_tree = |aligned: bool| {
            let mut form = Form::new()
                .comment("1  root — context")
                .checkboxes("", &["2  child — pick me"]);
            if aligned {
                form = form.aligned();
            }
            let rows = focusables(&form);
            render(&form, &rows, rows.len() - 1, 120)
                .iter()
                .map(|l| console::strip_ansi_codes(l).into_owned())
                .collect::<Vec<_>>()
        };
        let aligned = anonymous_tree(true);
        let comment = aligned.iter().find(|l| l.contains("root")).unwrap();
        let option = aligned.iter().find(|l| l.contains("child")).unwrap();
        assert_eq!(
            comment.find('1').unwrap(),
            option.find('2').unwrap(),
            "columns line up: {comment:?} vs {option:?}"
        );
        assert!(!aligned.iter().any(|l| l.trim_end().ends_with(':')), "anonymous group: no heading line");
        let plain = anonymous_tree(false);
        let comment = plain.iter().find(|l| l.contains("root")).unwrap();
        let option = plain.iter().find(|l| l.contains("child")).unwrap();
        assert_ne!(comment.find('1'), option.find('2'), "unaligned keeps the old compact look");
    }

    #[test]
    fn enter_submits_only_on_the_submit_row_and_esc_cancels_anywhere() {
        let mut form = form();
        let rows = focusables(&form);
        let mut focus = 0;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Escape), Action::Cancel);
        focus = rows.len() - 1;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Enter), Action::Submit);
    }

    #[test]
    fn the_render_shows_state_and_marks_exactly_one_focused_row() {
        let mut form = form().title("T");
        let Item::Checkboxes { checked, .. } = &mut form.items[1] else { panic!() };
        checked[1] = true;
        let Item::Radio { chosen, .. } = &mut form.items[2] else { panic!() };
        *chosen = Some(0);
        let rows = focusables(&form);
        let lines = render(&form, &rows, 0, 120);
        let plain: Vec<String> =
            lines.iter().map(|l| console::strip_ansi_codes(l).into_owned()).collect();
        let all = plain.join("\n");
        assert!(all.contains("[x] b") && all.contains("[ ] a"), "{all}");
        assert!(all.contains("(•) S") && all.contains("( ) M"), "{all}");
        assert!(all.contains("can't touch this"), "comments render, dimmed: {all}");
        assert!(all.contains("[ Submit ]"), "{all}");
        assert_eq!(
            plain.iter().filter(|line| line.starts_with("▸")).count(),
            1,
            "one focus marker: {all}"
        );
        // No line may exceed the width it was clipped for — wrapping breaks the redraw.
        let narrow = render(&form, &rows, 0, 24);
        for line in &narrow {
            let visible = console::measure_text_width(line);
            assert!(visible <= 24, "line of visible width {visible} would wrap: {line:?}");
        }
    }
}
