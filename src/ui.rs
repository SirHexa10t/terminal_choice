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
    /// Cell `column` of row `row` of the grid at `item`.
    Cell { item: usize, row: usize, column: usize },
    /// The text field at `item`.
    Text { item: usize },
    /// The final `[ Submit ]` row.
    Submit,
}

/// How every box stood when the form opened — what "you have changed this" is measured against.
///
/// Only a box that arrived TICKED and is now clear gets marked. The other direction is left
/// alone deliberately: a form that opens empty and is filled in would otherwise mark every
/// answer the user gives, which says nothing. Clearing something that arrived ticked is the
/// move worth a second look, because it reads as undoing a fact the form asserted.
///
/// Radios are not tracked. A radio cannot be cleared — picking moves it, and a move away from a
/// pre-chosen option is a choice like any other, not the reversal of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened(Vec<Vec<Vec<bool>>>);

impl Opened {
    /// Snapshot `form` as it stands. Taken once, when a run begins.
    ///
    /// Two dimensions throughout, so a grid and a checkbox group are read the same way: a
    /// checkbox group is simply a table one row deep.
    pub(crate) fn of(form: &Form) -> Self {
        Self(
            form.items
                .iter()
                .map(|item| match item {
                    Item::Checkboxes { checked, .. } => vec![checked.clone()],
                    Item::Grid { rows, .. } => rows
                        .iter()
                        .map(|row| row.cells.iter().map(|cell| cell.checked).collect())
                        .collect(),
                    _ => Vec::new(),
                })
                .collect(),
        )
    }

    /// Whether the box at `item`/`row`/`slot` arrived ticked and `now` is clear.
    fn cleared(&self, item: usize, row: usize, slot: usize, now: bool) -> bool {
        !now
            && self.0.get(item).and_then(|rows| rows.get(row)).and_then(|ticks| ticks.get(slot))
                == Some(&true)
    }
}

/// The focusable rows of `form`, in display order, always ending with Submit.
pub(crate) fn focusables(form: &Form) -> Vec<Focus> {
    let mut rows = Vec::new();
    for (index, item) in form.items.iter().enumerate() {
        match item {
            Item::Comment(_) => {}
            // A disabled option gets no row, which IS the "not selectable" guarantee — the same
            // way a comment's absence here is what makes comments unreachable. Nothing downstream
            // needs to check: no key can name a row that does not exist.
            Item::Checkboxes { enabled, .. } | Item::Radio { enabled, .. } => {
                rows.extend(
                    enabled
                        .iter()
                        .enumerate()
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| Focus::Option { item: index, option }),
                );
            }
            Item::Text { .. } => rows.push(Focus::Text { item: index }),
            // Row-major, so the flat order reads the way the table does — and so stepping one
            // place left or right lands in the neighbouring column of the same row.
            //
            // Every BOX gets a row, including a locked one. Unlike a checkbox, a locked cell is
            // worth resting on: it is the form saying "this could be had, but not yet", and the
            // preview under it is what that would take. Skipping them would also make sideways
            // movement do nothing on any row with a single live column, which is most of them on
            // a machine with one package manager. `enabled` is enforced where it belongs — on
            // the key that would change something.
            Item::Grid { rows: grid, .. } => {
                for (row, entry) in grid.iter().enumerate() {
                    rows.extend(entry.cells.iter().enumerate().filter(|(_, cell)| cell.boxed).map(
                        |(column, _)| Focus::Cell { item: index, row, column },
                    ));
                }
            }
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
            // Only the boxes that are the user's to change: "all" cannot mean reaching past a
            // disabled one, and a disabled box must not decide whether "all" is already true.
            let mut boxes = 0;
            let mut ticked = 0;
            for item in &form.items {
                if let Item::Checkboxes { checked, enabled, .. } = item {
                    boxes += enabled.iter().filter(|on| **on).count();
                    ticked += checked.iter().zip(enabled).filter(|(on, live)| **on && **live).count();
                }
            }
            if boxes == 0 {
                return Action::Ignored;
            }
            let everything_on = ticked == boxes;
            for item in &mut form.items {
                if let Item::Checkboxes { checked, enabled, .. } = item {
                    for (on, live) in checked.iter_mut().zip(enabled) {
                        if *live {
                            *on = !everything_on;
                        }
                    }
                }
            }
        }
        // In a grid the arrows mean what they look like: sideways moves along a row, up and
        // down moves between rows keeping as near the same column as that row allows. Outside
        // one, and when a grid has no further row that way, they fall back to walking the flat
        // list — which is how the cursor gets out of a table and down to Submit.
        Key::ArrowLeft | Key::ArrowRight => {
            let forward = key == Key::ArrowRight;
            match _along_row(form, rows, *focus, forward) {
                Some(next) => *focus = next,
                None => return Action::Ignored,
            }
        }
        Key::ArrowDown | Key::Tab => {
            *focus = _across_rows(form, rows, *focus, true)
                .unwrap_or_else(|| (*focus + 1) % rows.len());
        }
        Key::ArrowUp => {
            *focus = _across_rows(form, rows, *focus, false)
                .unwrap_or_else(|| (*focus + rows.len() - 1) % rows.len());
        }
        Key::Enter => match rows[*focus] {
            Focus::Submit => return Action::Submit,
            _ => *focus = (*focus + 1) % rows.len(),
        },
        Key::Char(' ') if !editing_text => {
            if let Focus::Cell { item, row, column } = rows[*focus] {
                if let Item::Grid { rows: grid, .. } = &mut form.items[item] {
                    let cell = &mut grid[row].cells[column];
                    // The cursor may rest here; changing it is another matter.
                    if !cell.enabled {
                        return Action::Ignored;
                    }
                    cell.checked = !cell.checked;
                }
                return Action::Redraw;
            }
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
                        if let Item::Checkboxes { options, checked, enabled, .. } = item {
                            for (slot, text) in options.iter().enumerate() {
                                // A twin the user cannot touch is not moved by touching its
                                // sibling either — "not yours to change" holds from every angle.
                                if *text == name && enabled[slot] {
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

/// The next focusable cell along the SAME grid row, in the given direction — `None` when the
/// cursor is not in a grid, or has run out of row.
///
/// Sideways movement never leaves the row it started in. Wrapping to the next row would make a
/// grid behave like a flat list that happens to be drawn in a table, which is the one thing a
/// table is not.
fn _along_row(form: &Form, rows: &[Focus], focus: usize, forward: bool) -> Option<usize> {
    let Focus::Cell { item, row, .. } = rows[focus] else { return None };
    let step: Box<dyn Iterator<Item = usize>> = match forward {
        true => Box::new(focus + 1..rows.len()),
        false => Box::new((0..focus).rev()),
    };
    let _ = form;
    step.take_while(|at| matches!(rows[*at], Focus::Cell { item: i, row: r, .. } if i == item && r == row))
        .next()
}

/// The focusable cell nearest the current column, one grid row up or down — `None` when the
/// cursor is not in a grid, or the grid has no further row that way.
///
/// Rows with no box at all are stepped over rather than stopping the cursor, and the landing
/// column is the nearest BOX to where the cursor already was: a table that threw the cursor back
/// to column one on every vertical move would be unusable with ten columns.
fn _across_rows(form: &Form, rows: &[Focus], focus: usize, down: bool) -> Option<usize> {
    let Focus::Cell { item, row, column } = rows[focus] else { return None };
    let Item::Grid { rows: grid, .. } = &form.items[item] else { return None };

    let mut candidate = row;
    loop {
        candidate = match down {
            true => candidate.checked_add(1)?,
            false => candidate.checked_sub(1)?,
        };
        let entry = grid.get(candidate)?;
        let nearest = entry
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.boxed)
            .min_by_key(|(at, _)| (at.abs_diff(column), *at))
            .map(|(at, _)| at);
        if let Some(landing) = nearest {
            return rows
                .iter()
                .position(|at| *at == Focus::Cell { item, row: candidate, column: landing });
        }
    }
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
pub(crate) fn render(
    form: &Form,
    rows: &[Focus],
    focus: usize,
    width: usize,
    warnings: &[String],
    opened: &Opened,
) -> Vec<String> {
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
            Item::Checkboxes { label, options, checked, headings, enabled } => {
                // An anonymous group draws no heading — it exists to sit flush inside
                // surrounding comments (a tree of rows, some of them tickable).
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, name) in options.iter().enumerate() {
                    lines.extend(subtitle(headings, option, comment_indent, &clean));
                    let box_mark = if checked[option] { "[x]" } else { "[ ]" };
                    let row = format!("{box_mark} {}", clean(name.clone()));
                    // Red BEFORE the focus mark, so that `mark`'s re-arming carries the colour's
                    // reset through the reverse video rather than being cut short by it.
                    let row = match opened.cleared(index, 0, option, checked[option]) {
                        true => console::style(row).red().to_string(),
                        false => row,
                    };
                    lines.push(match enabled[option] {
                        // Dim, and never marked: the focus list has no row for it, so `mark`
                        // could not report it focused anyway — this only says so visibly.
                        false => console::style(format!("  {row}")).dim().to_string(),
                        true => mark(row, Focus::Option { item: index, option }),
                    });
                }
            }
            Item::Radio { label, options, chosen, headings, enabled } => {
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, name) in options.iter().enumerate() {
                    lines.extend(subtitle(headings, option, comment_indent, &clean));
                    let dot = if *chosen == Some(option) { "(•)" } else { "( )" };
                    let row = format!("{dot} {}", clean(name.clone()));
                    lines.push(match enabled[option] {
                        false => console::style(format!("  {row}")).dim().to_string(),
                        true => mark(row, Focus::Option { item: index, option }),
                    });
                }
            }
            Item::Text { label, value } => {
                lines.push(mark(format!("{label}: {value}▏"), Focus::Text { item: index }));
            }
            Item::Grid { label, columns, rows: grid } => {
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                // Each column as wide as ITS OWN heading needs, never as wide as the widest:
                // one `nix-env` would otherwise push every other column three spaces apart. No
                // abbreviating either — the heading is the only place a column says which it is,
                // and `nix` cut to three characters is a different manager from `nix-env`.
                let pad = |text: &str, width: usize| {
                    " ".repeat(width.saturating_sub(console::measure_text_width(text)))
                };
                let slots: Vec<usize> = columns
                    .iter()
                    .map(|column| console::measure_text_width(column).max(BOX) + GAP)
                    .collect();
                let heads: String = columns
                    .iter()
                    .zip(&slots)
                    .map(|(column, slot)| format!("{column}{}", pad(column, *slot)))
                    .collect();
                lines.push(console::style(heads).dim().to_string());

                // Labels align into a column, so the notes after them do too — a ragged right
                // edge of `#` remarks is harder to read past than no remarks at all.
                let widest = grid
                    .iter()
                    .map(|row| console::measure_text_width(&row.label))
                    .max()
                    .unwrap_or(0);
                for (row, entry) in grid.iter().enumerate() {
                    lines.extend(subtitle(std::slice::from_ref(&entry.heading), 0, "", &clean));
                    let boxes: String = entry
                        .cells
                        .iter()
                        .enumerate()
                        .map(|(column, cell)| {
                            let drawn = match cell.checked {
                                true => "[x]",
                                false => "[ ]",
                            };
                            // Padding is added AFTER any styling, so an escape never counts
                            // towards the width and the columns stay straight.
                            let inked = match (cell.boxed, cell.enabled) {
                                // Nothing this column could ever do for this row.
                                (false, _) => console::style(" · ").dim().to_string(),
                                // A choice that exists but is out of reach — shown as the box it
                                // is, so a reader can see what setting something up would unlock.
                                (true, false) => console::style(drawn).dim().to_string(),
                                (true, true) => match opened.cleared(index, row, column, cell.checked) {
                                    true => console::style(drawn).red().to_string(),
                                    false => drawn.to_string(),
                                },
                            };
                            let inked = match rows[focus] == (Focus::Cell { item: index, row, column }) {
                                true => format!("\x1b[7m{}\x1b[0m", inked.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
                                false => inked,
                            };
                            format!("{inked}{}", pad(drawn, slots[column]))
                        })
                        .collect();
                    let named = clean(entry.label.clone());
                    let note = entry.note.as_ref().map_or(String::new(), |note| {
                        let gap = pad(&named, widest);
                        console::style(format!("{gap}  # {}", clean(note.clone()))).dim().to_string()
                    });
                    lines.push(format!("{boxes}{named}{note}"));
                }
            }
        }
    }
    lines.push(String::new());
    lines.push(mark("[ Submit ]".to_string(), Focus::Submit));
    // What the cell under the cursor would do, if it says. Above the cautions because it is
    // about the one thing being looked at, while they are about the whole answer.
    if let Some(said) = _preview(form, rows.get(focus)) {
        for line in _wrap(&clean(said), width.saturating_sub(4)) {
            lines.push(format!("  → {line}"));
        }
    }
    // Cautions sit between Submit and the key hints: under the thing they are a caution ABOUT,
    // where the eye already is before pressing it, and above the hints so that several of them
    // push the hints down the screen rather than scrolling themselves off it.
    for warning in warnings {
        for (line, text) in _wrap(&clean(warning.clone()), width.saturating_sub(4)).iter().enumerate()
        {
            // Continuations hang under the first line's text, not under its marker.
            let lead = if line == 0 { "  ⚠ " } else { "    " };
            lines.push(console::style(format!("{lead}{text}")).red().to_string());
        }
    }
    lines.push(
        console::style("↑/↓/←/→ move · space picks · ctrl+a all/none · enter next/submit · esc cancels")
            .dim()
            .to_string(),
    );
    lines.into_iter().map(|line| console::truncate_str(&line, width, "…").into_owned()).collect()
}

/// A sub-title standing above option `slot`, as rendered lines — dim, like a comment, because
/// that is what it is: the file's own words about the options beneath it. Empty when the option
/// carries none, which is most of them.
fn subtitle(
    headings: &[Option<String>],
    slot: usize,
    indent: &str,
    clean: &impl Fn(String) -> String,
) -> Vec<String> {
    let Some(Some(said)) = headings.get(slot) else {
        return Vec::new();
    };
    said.lines()
        .map(|line| console::style(format!("{indent}{}", clean(line.to_string()))).dim().to_string())
        .collect()
}

/// The cautions one repaint shows, from both doors: what the definition declared, in its own
/// order, then whatever this run's caller adds on top. Split out from the loop so the composition
/// is testable without a terminal, like everything else that decides what appears.
fn _notes(form: &mut Form, warn: &mut impl FnMut(&mut Form) -> Vec<String>) -> Vec<String> {
    // The hook runs FIRST: it may restyle rows, and a declared warning read before that would
    // describe a form that is about to change on screen.
    let supplied = warn(form);
    let mut notes: Vec<String> = form.active_warnings().into_iter().map(String::from).collect();
    notes.extend(supplied);
    notes
}

/// How wide a grid box is drawn.
const BOX: usize = 3;

/// What separates one grid column from the next. Two spaces, the same gap a table uses — one
/// leaves `guix nix-env` reading as a single word.
const GAP: usize = 2;

/// What the focused cell would do: the line that fills it while it is empty, the line that
/// empties it while it is full. Which way round matters — over a ticked box the interesting
/// command is the one that would undo it, not the one that already ran.
fn _preview(form: &Form, focus: Option<&Focus>) -> Option<String> {
    let Focus::Cell { item, row, column } = focus? else { return None };
    let Item::Grid { columns, rows, .. } = form.items.get(*item)? else { return None };
    let cell = rows.get(*row)?.cells.get(*column)?;
    let said = match cell.checked {
        true => cell.on_clear.as_ref(),
        false => cell.on_set.as_ref(),
    }?;
    // The heading is truncated in the table, so name the column in full here — this line is
    // where a reader finds out which of ten columns the cursor is actually in.
    let named = columns.get(*column).map_or(String::new(), |column| format!("{column}: "));
    Some(format!("{named}{said}"))
}

/// `text` broken at spaces into lines of at most `width` display cells. Warnings are prose, and
/// prose that runs off the edge is a warning nobody reads — every other row here is a label or an
/// option, short by nature, and truncation serves those fine.
///
/// A word wider than the budget still gets its own line rather than being split mid-glyph; the
/// caller's final truncation pass trims it. Empty lines never survive, so a blank warning renders
/// as nothing at all instead of a red gap.
fn _wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("seeded with one line");
        let grown = console::measure_text_width(line) + 1 + console::measure_text_width(word);
        if !line.is_empty() && grown > width {
            lines.push(word.to_string());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines.retain(|line| !line.is_empty());
    lines
}

/// Run `form` interactively on the terminal; the form's own state carries the answers.
///
/// Drawn on stderr, so a scripted caller can pipe stdout (where a binary prints the answers)
/// while the form still appears. Not a terminal → an error naming the problem, not a hang.
pub fn run(form: &mut Form) -> std::io::Result<Outcome> {
    run_with_warnings(form, |_| Vec::new())
}

/// [`run`], plus a line of red under the form for each string `warn` returns — on top of the
/// form's own declared warnings, which [`run`] shows too.
///
/// `warn` is the per-repaint hook: called fresh every time the form is drawn, with the form as it
/// stands. What it says is entirely the caller's business, and nothing here constrains it — it
/// may consult the machine, the filesystem, its own tables. That is the point of a closure over
/// the file vocabulary in [`crate::Condition`]: "this path already exists", "that needs a reboot"
/// are not things a form library can be taught, and any program should warn for its own reasons.
///
/// It takes the form by `&mut` so it can also change how the form LOOKS before that repaint —
/// option text carries ANSI, so a caller marks the rows it wants noticed by rewriting them. Do
/// not change ANSWERS from here: ticking a box the user did not tick moves the form under their
/// hands, and they have no way to know it happened.
///
/// Warnings never gate anything. Submitting is still allowed while every one of them shows —
/// they exist to say "this is unusual" before the user commits to it, not to refuse.
///
/// The closure is passed to the RUN rather than stored on the [`Form`], which is what lets a
/// form stay `Clone`, `PartialEq` and loadable from TOML: a boxed function is none of those.
///
/// ```no_run
/// # use terminal_choice::{Form, run_with_warnings};
/// let mut form = Form::new().checkboxes("packages", &["mullvad", "wireguard", "zed"]);
/// let vpns = ["mullvad", "wireguard"];
/// run_with_warnings(&mut form, |form| {
///     let picked = form.checked("packages");
///     match picked.iter().filter(|name| vpns.contains(name)).count() >= 2 {
///         true => vec!["More than one VPN client — unusual, but allowed.".to_string()],
///         false => Vec::new(),
///     }
/// })?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn run_with_warnings(
    form: &mut Form,
    mut warn: impl FnMut(&mut Form) -> Vec<String>,
) -> std::io::Result<Outcome> {
    let term = Term::stderr();
    if !term.is_term() {
        return Err(std::io::Error::other(
            "interactive forms need a terminal (stderr is not one)",
        ));
    }
    let rows = focusables(form);
    // Taken once, before a key is pressed: the whole point is to compare against what the CALLER
    // handed over, not against whatever the last repaint happened to see.
    let opened = Opened::of(form);
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
        // Asked again every repaint, so the cautions — and any emphasis the caller paints on
        // the rows — track the answers as they change.
        let notes = _notes(form, &mut warn);
        let lines = render(form, &rows, focus, width.max(20), &notes, &opened);
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
    use crate::{Condition, GridCell, GridRow};

    fn form() -> Form {
        Form::new()
            .comment("can't touch this")
            .checkboxes("Tops", &["a", "b"])
            .radio("Size", &["S", "M"])
            .text("Name", "")
    }

    /// The two doors compose: what a definition declared, then what this run's caller adds. A
    /// program is never made to choose between them, and neither can silence the other.
    #[test]
    fn declared_and_supplied_warnings_both_show() {
        let mut form = Form::new().checkboxes("Tops", &["a", "b"]).warning(
            "declared: both ticked",
            Condition::Checked { label: "Tops".into(), options: vec!["a".into(), "b".into()] },
        );
        let supplied = |form: &mut Form| match form.checked("Tops").is_empty() {
            true => vec!["supplied: nothing chosen".to_string()],
            false => Vec::new(),
        };
        let none = |_: &mut Form| Vec::new();

        // Only the closure's, at first — the declared rule needs both boxes.
        assert_eq!(_notes(&mut form, &mut supplied.clone()), ["supplied: nothing chosen"]);
        assert!(
            _notes(&mut form, &mut none.clone()).is_empty(),
            "a plain run shows only what was declared"
        );

        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = true;
        checked[1] = true;
        // Now only the declared one — and it shows even with an empty closure, which is what
        // `run` passes, so a TOML-defined form warns without any code at all.
        assert_eq!(_notes(&mut form, &mut supplied.clone()), ["declared: both ticked"]);
        assert_eq!(_notes(&mut form, &mut none.clone()), ["declared: both ticked"]);

        // Both at once, declared first.
        let mut always = |_: &mut Form| vec!["supplied: always".to_string()];
        assert_eq!(_notes(&mut form, &mut always), ["declared: both ticked", "supplied: always"]);
    }

    /// Warnings are the CALLER's judgement, drawn here and nowhere decided here: `render` takes
    /// finished text. They land between Submit and the key hints, in red, and prose too wide for
    /// the terminal wraps rather than being cut — a warning nobody can read is not one.
    #[test]
    fn warnings_draw_in_red_between_submit_and_the_hints() {
        let form = form();
        let rows = focusables(&form);
        let plain = |lines: &[String]| -> Vec<String> {
            lines.iter().map(|l| console::strip_ansi_codes(l).into_owned()).collect()
        };

        // Nothing supplied, nothing drawn: the block costs a form that wants none of it nothing.
        let quiet = plain(&render(&form, &rows, 0, 40, &[], &Opened::of(&form)));
        assert!(!quiet.iter().any(|line| line.contains('\u{26a0}')), "{quiet:?}");

        let said = "Two VPN clients at once is unusual, but you are allowed to do it anyway.";
        let loud = render(&form, &rows, 0, 40, &[said.to_string()], &Opened::of(&form));
        let flat = plain(&loud);

        let first = flat.iter().position(|line| line.contains('\u{26a0}')).expect("a warning shows");
        let submit = flat.iter().position(|line| line.contains("[ Submit ]")).expect("submit");
        let hints = flat.iter().position(|line| line.contains("\u{2191}/\u{2193}")).expect("hints");
        assert!(submit < first && first < hints, "warning sits between them: {flat:?}");
        // Red — asserted against the same styling call, so this holds whether or not colours are
        // enabled here. Under NO_COLOR (or a pipe) the marker still carries the meaning alone.
        assert_eq!(
            loud[first],
            console::style("  \u{26a0} Two VPN clients at once is unusual,").red().to_string(),
            "the warning line is the red-styled first wrap"
        );

        // Wrapped, not truncated, and the sentence survives whole across the lines.
        let block: Vec<&String> = flat[first..hints].iter().collect();
        assert!(block.len() > 1, "40 columns cannot hold that sentence: {block:?}");
        assert!(block.iter().all(|line| !line.contains('\u{2026}')), "wrapped: {block:?}");
        assert!(block[1].starts_with("    ") && !block[1].contains('\u{26a0}'), "hangs: {:?}", block[1]);
        let rejoined: String = block.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, format!("\u{26a0} {said}"));
    }

    /// What `run` does every repaint, without a terminal: ask the caller again. The point of a
    /// closure over a stored rule is that the answer may depend on anything at all — here, on
    /// the form's own state, which is the case a fixed condition vocabulary would have covered
    /// and the many that it would not.
    #[test]
    fn the_caller_is_asked_again_on_every_repaint() {
        let mut form = form();
        let rows = focusables(&form);
        let vpns = ["a", "b"];
        let warn = |form: &Form| -> Vec<String> {
            let picked = form.checked("Tops");
            match picked.iter().filter(|name| vpns.contains(name)).count() >= 2 {
                true => vec!["Two at once.".to_string()],
                false => Vec::new(),
            }
        };
        let showing = |form: &Form| {
            render(form, &rows, 0, 60, &warn(form), &Opened::of(form))
                .iter()
                .any(|line| console::strip_ansi_codes(line).contains("Two at once."))
        };

        assert!(!showing(&form), "nothing ticked yet");
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Char(' ')); // tick "a"
        assert!(!showing(&form), "one is a normal answer");
        focus = 1;
        apply(&mut form, &rows, &mut focus, Key::Char(' ')); // tick "b"
        assert!(showing(&form), "the second tick brings it out");
        apply(&mut form, &rows, &mut focus, Key::Char(' ')); // untick "b"
        assert!(!showing(&form), "and unticking takes it away again");
    }

    /// "Not selectable" is enforced by absence, not by a check: a disabled option has no focus
    /// row, so no key can name it. Ctrl+A and duplicate-mirroring reach boxes WITHOUT the focus
    /// list, though, so each is verified separately — those are the two ways past the guarantee.
    #[test]
    fn a_disabled_option_cannot_be_reached_by_any_key() {
        let mut form = Form::new().checkboxes("Tops", &["a", "b", "c"]);
        let Item::Checkboxes { enabled, checked, .. } = &mut form.items[0] else { panic!() };
        enabled[1] = false;
        checked[1] = true; // it arrives ticked, and must stay ticked

        let rows = focusables(&form);
        assert_eq!(
            rows,
            [
                Focus::Option { item: 0, option: 0 },
                Focus::Option { item: 0, option: 2 },
                Focus::Submit
            ],
            "the disabled option has no row at all"
        );

        // Space, everywhere it can land: never reaches option 1.
        let mut focus = 0;
        for _ in 0..rows.len() * 2 {
            apply(&mut form, &rows, &mut focus, Key::Char(' '));
            apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        }
        let Item::Checkboxes { checked, .. } = &form.items[0] else { panic!() };
        assert!(checked[1], "a fixed box keeps its state through every keystroke");

        // Ctrl+A: flips what is the user's, leaves what is not.
        let mut form = Form::new().checkboxes("Tops", &["a", "b", "c"]);
        let Item::Checkboxes { enabled, .. } = &mut form.items[0] else { panic!() };
        enabled[1] = false;
        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { checked, .. } = &form.items[0] else { panic!() };
        assert_eq!(checked, &[true, false, true], "all-on skips the fixed box");
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { checked, .. } = &form.items[0] else { panic!() };
        assert_eq!(
            checked,
            &[false, false, false],
            "every box it may touch was on, so the second press clears those and only those"
        );
    }

    /// Mirroring reaches boxes by NAME, in every group at once — so a disabled twin is the one
    /// place a locked box could be moved by touching something else. It is not.
    #[test]
    fn mirroring_does_not_move_a_locked_twin() {
        let mut form =
            Form::new().checkboxes("Left", &["shared"]).checkboxes("Right", &["shared"]);
        form.mirror_duplicates = true;
        let Item::Checkboxes { enabled, .. } = &mut form.items[1] else { panic!() };
        enabled[0] = false;

        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Char(' '));

        let Item::Checkboxes { checked, .. } = &form.items[0] else { panic!() };
        assert!(checked[0], "the one that was ticked");
        let Item::Checkboxes { checked, .. } = &form.items[1] else { panic!() };
        assert!(!checked[0], "its locked twin stayed put");
    }

    /// Sub-titles draw dim above the option they belong to, and a disabled option draws dim
    /// itself — the two ways this form says "read, do not touch".
    #[test]
    fn sub_titles_and_locked_options_are_drawn_dim() {
        let mut form = Form::new().checkboxes("Packages", &["zed", "helix", "apt"]);
        let Item::Checkboxes { headings, enabled, .. } = &mut form.items[0] else { panic!() };
        headings[0] = Some("dev-tools".into());
        headings[2] = Some("system\n(not yours)".into());
        enabled[2] = false;

        let rows = focusables(&form);
        let drawn = render(&form, &rows, 0, 100, &[], &Opened::of(&form));
        let flat: Vec<String> =
            drawn.iter().map(|l| console::strip_ansi_codes(l).trim_end().to_string()).collect();

        assert_eq!(
            flat,
            [
                "Packages:",
                "  dev-tools",
                "▸ [ ] zed",
                "  [ ] helix",
                "  system",
                "  (not yours)",
                "  [ ] apt",
                "",
                "  [ Submit ]",
                "↑/↓/←/→ move · space picks · ctrl+a all/none · enter next/submit · esc cancels",
            ],
            "{flat:#?}"
        );
        // The locked row is styled like a comment, not like a focusable one.
        let locked = drawn.iter().find(|l| l.contains("apt")).expect("drawn");
        assert_eq!(locked, &console::style("  [ ] apt").dim().to_string());
    }

    /// A box the form opened with, and the user has since cleared, is drawn red — clearing a tick
    /// the form asserted reads as undoing a fact, and is worth a second look before submitting.
    /// The other direction is deliberately silent: a form that opens empty and gets filled in
    /// would otherwise mark every answer the user gives, which says nothing at all.
    #[test]
    fn clearing_a_box_the_form_arrived_with_marks_it_red() {
        let mut form = Form::new().checkboxes("Settings", &["on", "off"]);
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = true;

        let opened = Opened::of(&form); // taken once, as a run does
        let rows = focusables(&form);
        let row = |form: &Form, want: &str| {
            render(form, &rows, rows.len() - 1, 60, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };

        // Focus parks on Submit throughout, so nothing here is the focus highlight's doing.
        assert_eq!(row(&form, "on"), "  [x] on", "unchanged, unmarked");
        assert_eq!(row(&form, "off"), "  [ ] off");

        // Clear the one that arrived ticked.
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = false;
        assert_eq!(row(&form, "on"), console::style("  [ ] on").red().to_string());

        // Tick the one that arrived clear: a plain answer, not a reversal.
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[1] = true;
        assert_eq!(row(&form, "off"), "  [x] off", "filling something in is not undoing it");

        // Put it back: the mark comes off as cleanly as it went on.
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = true;
        assert_eq!(row(&form, "on"), "  [x] on");
    }

    /// Three managers, three packages, one unavailable everywhere but the middle — the shape the
    /// whole feature exists for. Boxes first so the columns read straight down, name after.
    fn grid_form() -> Form {
        let cell = |on: bool, live: bool| match (on, live) {
            (_, false) => GridCell::blank(),
            (true, true) => GridCell::set(Some("remove it".into())),
            (false, true) => GridCell::open(Some("install it".into())),
        };
        Form::new().grid(
            "packages",
            &["apt", "flatpak", "snap"],
            vec![
                GridRow {
                    label: "git".into(),
                    heading: Some("# tools".into()),
                    note: Some("version control".into()),
                    cells: vec![cell(true, true), GridCell::blank(), GridCell::blank()],
                },
                GridRow {
                    label: "brave".into(),
                    heading: Some("# browsers".into()),
                    note: None,
                    cells: vec![GridCell::blank(), cell(true, true), cell(false, true)],
                },
                GridRow {
                    label: "firefox".into(),
                    heading: None,
                    note: None,
                    cells: vec![cell(false, true), cell(false, true), cell(false, true)],
                },
            ],
        )
    }

    /// The layout: a heading row of truncated column names, a sub-title where one was given,
    /// then one line per row — boxes, then the label. Unavailable cells are dim and not boxes at
    /// all, so an eye scanning a column can tell "no" from "not offered".
    #[test]
    fn a_grid_draws_as_a_table_with_the_names_after_the_boxes() {
        let form = grid_form();
        let rows = focusables(&form);
        let drawn: Vec<String> = render(&form, &rows, 0, 60, &[], &Opened::of(&form))
            .iter()
            .map(|line| console::strip_ansi_codes(line).trim_end().to_string())
            .collect();

        assert_eq!(
            &drawn[..7],
            [
                "packages:",
                "apt  flatpak  snap",
                "# tools",
                "[x]   ·        ·    git      # version control",
                "# browsers",
                " ·   [x]      [ ]   brave",
                "[ ]  [ ]      [ ]   firefox",
            ],
            "{drawn:#?}"
        );
        // Every box starts where its heading does — the property that makes a column readable as
        // a column, and the reason the slot is as wide as the longest NAME rather than the boxes
        // being packed and the names cut to fit.
        // Each column is as wide as ITS OWN heading, so `apt` does not get spaced out to
        // `flatpak`'s width — and every box still starts where its heading does.
        let heading = drawn[1].find("flatpak").expect("second heading");
        assert_eq!(drawn[6].find("[ ]  [ ]").map(|at| at + 5), Some(heading), "{drawn:#?}");
    }

    /// Sideways moves stay in their row; up and down move between rows, landing as near the
    /// column the cursor was in as that row allows — and step over a cell nothing offers.
    #[test]
    fn the_arrows_mean_what_they_look_like() {
        let mut form = grid_form();
        let rows = focusables(&form);
        let at = |focus: usize| rows[focus];
        let mut focus = 0;

        // Row 0 has one live cell: sideways does nothing rather than wrapping into row 1.
        assert_eq!(at(focus), Focus::Cell { item: 0, row: 0, column: 0 });
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::ArrowRight), Action::Ignored);
        assert_eq!(at(focus), Focus::Cell { item: 0, row: 0, column: 0 });

        // Down from column 0 of row 0: row 1 has nothing in column 0, so the nearest live one.
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(at(focus), Focus::Cell { item: 0, row: 1, column: 1 });
        apply(&mut form, &rows, &mut focus, Key::ArrowRight);
        assert_eq!(at(focus), Focus::Cell { item: 0, row: 1, column: 2 });

        // Down keeps the column when the next row has it.
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(at(focus), Focus::Cell { item: 0, row: 2, column: 2 });

        // Past the last row, the arrows fall back to the flat list — which is how the cursor
        // reaches Submit at all.
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(at(focus), Focus::Submit);
    }

    /// A cell nothing offers has no focus row, so no key can reach it — the same guarantee a
    /// disabled option has, by the same mechanism.
    #[test]
    fn an_unavailable_cell_cannot_be_reached_or_changed() {
        let mut form = grid_form();
        let rows = focusables(&form);
        assert!(
            !rows.contains(&Focus::Cell { item: 0, row: 0, column: 1 }),
            "row 0 offers only apt: {rows:?}"
        );
        assert_eq!(rows.len(), 7, "one live cell, two, three, and Submit: {rows:?}");

        let mut focus = 0;
        for _ in 0..24 {
            apply(&mut form, &rows, &mut focus, Key::Char(' '));
            apply(&mut form, &rows, &mut focus, Key::ArrowDown);
            apply(&mut form, &rows, &mut focus, Key::ArrowRight);
        }
        let Item::Grid { rows: grid, .. } = &form.items[0] else { panic!() };
        assert!(!grid[0].cells[1].checked, "a shut cell stayed shut through every keystroke");
        assert!(!grid[0].cells[2].checked);
    }

    /// The preview names the column in full — the heading above is three characters — and says
    /// what the box would DO next, which flips with the box: over a ticked one the interesting
    /// line is the one that would undo it.
    #[test]
    fn the_preview_says_what_this_cell_would_do_next() {
        let mut form = grid_form();
        let rows = focusables(&form);
        let shown = |form: &Form, focus: usize| {
            render(form, &rows, focus, 60, &[], &Opened::of(form))
                .iter()
                .map(|line| console::strip_ansi_codes(line).trim_end().to_string())
                .find(|line| line.starts_with("  →"))
        };

        // Cursor on git/apt, which is ticked: the line that would clear it.
        assert_eq!(shown(&form, 0).as_deref(), Some("  → apt: remove it"));
        // brave/snap is clear: the line that would fill it.
        let snap = rows.iter().position(|at| *at == Focus::Cell { item: 0, row: 1, column: 2 });
        assert_eq!(shown(&form, snap.unwrap()).as_deref(), Some("  → snap: install it"));

        // Ticking it flips which line is interesting.
        let mut focus = snap.unwrap();
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(shown(&form, focus), None, "an open cell was given no clearing line");

        // Off the grid entirely, there is nothing to preview.
        assert_eq!(shown(&form, rows.len() - 1), None, "the cursor is on Submit");
    }

    /// A cell that arrived ticked and has been cleared goes red, exactly as a checkbox does —
    /// which is what lets "I no longer want this" be seen while it is being said.
    #[test]
    fn clearing_a_cell_the_grid_arrived_with_marks_it_red() {
        let mut form = grid_form();
        let rows = focusables(&form);
        let opened = Opened::of(&form);
        let git_row = |form: &Form| {
            render(form, &rows, rows.len() - 1, 60, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains("git"))
                .expect("drawn")
        };
        assert_eq!(
            git_row(&form),
            "[x]   ·        ·    git      # version control",
            "unchanged, unmarked"
        );

        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Char(' ')); // clear git/apt
        // Asserted against the same styling call, so this holds whether or not colours are
        // enabled here — under NO_COLOR the box is plain and the row is otherwise identical.
        assert_eq!(
            git_row(&form),
            format!("{}   ·        ·    git      # version control", console::style("[ ]").red()),
            "a cell that arrived ticked and was cleared is marked"
        );
    }

    /// The answers come back two-dimensional, because the question was: which rows, through
    /// which columns. A row nothing was ticked in is left out rather than written empty.
    #[test]
    fn a_grid_answers_in_rows_and_columns() {
        let form = grid_form();
        let answers = form.answers_toml();
        assert!(answers.contains("[packages]"), "{answers}");
        assert!(answers.contains(r#"git = ["apt"]"#), "{answers}");
        assert!(answers.contains(r#"brave = ["flatpak"]"#), "{answers}");
        assert!(!answers.contains("firefox"), "nothing ticked, nothing said: {answers}");
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
        let lines = render(&coloured, &rows, 0, 200, &[], &Opened::of(&coloured));
        let focused = lines.iter().find(|l| l.contains("firefox")).unwrap();
        assert!(focused.contains("\x1b[30;41m"), "the caller's colours are kept: {focused:?}");
        assert!(
            focused.contains("\x1b[0m\x1b[7m"),
            "every embedded reset re-arms the focus reverse: {focused:?}"
        );
        let scrubbed = Form::new().checkboxes("Pick", &[glow]).scrub_colors();
        let rows = focusables(&scrubbed);
        let lines = render(&scrubbed, &rows, 0, 200, &[], &Opened::of(&scrubbed));
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
            render(&form, &rows, rows.len() - 1, 120, &[], &Opened::of(&form))
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
        let lines = render(&form, &rows, 0, 120, &[], &Opened::of(&form));
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
        let narrow = render(&form, &rows, 0, 24, &[], &Opened::of(&form));
        for line in &narrow {
            let visible = console::measure_text_width(line);
            assert!(visible <= 24, "line of visible width {visible} would wrap: {line:?}");
        }
    }
}
