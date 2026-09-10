//! The interactive part, built to be tested without a terminal: `focusables` maps a form to
//! its interactive rows, `render` draws the whole form as plain lines, and `apply` is the
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
    /// A foldable sub-title of `item`, standing above `slot`. Only ever present while
    /// [`Form::collapsible`] is set.
    ///
    /// `foot` distinguishes the two rows an OPEN section draws: the `v` above its options and the
    /// `^` below them. Both fold it, which is the point — a section long enough to want folding
    /// is long enough that scrolling back to its top to do so is a nuisance. A shut section draws
    /// one row, `>`, and that one is never the foot.
    Section { item: usize, slot: usize, foot: bool },
    /// Tick-box `at` of the filter block — see [`Form::filters`]. Above every item, because it
    /// governs all of them.
    Filter { at: usize },
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
    /// A SUGGESTED tick is recorded as clear, which is the whole of what makes a suggestion one.
    /// The form arrived showing it, but it asserts nothing — so declining it is an ordinary
    /// answer, not the undoing of a fact, and nothing here has anything to mark.
    pub(crate) fn of(form: &Form) -> Self {
        Self(
            form.items
                .iter()
                .map(|item| match item {
                    Item::Checkboxes { options, .. } => {
                        vec![options.iter().map(|o| o.checked && !o.suggested).collect()]
                    }
                    Item::Grid { rows, .. } => rows
                        .iter()
                        .map(|row| {
                            row.cells.iter().map(|cell| cell.checked && !cell.suggested).collect()
                        })
                        .collect(),
                    _ => Vec::new(),
                })
                .collect(),
        )
    }

    /// Whether the box at `item`/`row`/`slot` arrived ticked — a fact about the machine, which is
    /// what earns it the [`GIVEN`] glyph while it stands and the red mark when it is cleared.
    fn ticked(&self, item: usize, row: usize, slot: usize) -> bool {
        self.0.get(item).and_then(|rows| rows.get(row)).and_then(|ticks| ticks.get(slot)) == Some(&true)
    }

    /// Whether the box at `item`/`row`/`slot` arrived ticked and `now` is clear.
    fn cleared(&self, item: usize, row: usize, slot: usize, now: bool) -> bool {
        !now && self.ticked(item, row, slot)
    }
}

/// The fold row, if any, that belongs immediately before slot `slot` of `item` (`foot` false) or
/// immediately after it (`foot` true).
///
/// One function for both ends so the head and the foot can never drift apart: a section draws a
/// `^` exactly when it drew a `v`, at exactly the slot its section runs out. Yields nothing at
/// all unless the form folds, which is what keeps every existing form's focus list identical.
fn _folds(form: &Form, item: usize, slot: usize, foot: bool) -> Option<Focus> {
    if !form.collapsible {
        return None;
    }
    let head = match foot {
        // The `v`/`>` sits above the slot that carries the heading.
        false => form.heading_at(item, slot).is_some().then_some(slot),
        // The `^` sits after the last slot of an OPEN section. A shut one drew no options, so
        // there is nothing for a closing marker to close.
        true => form
            .section_head(item, slot)
            .filter(|head| !form.collapsed.contains(&(item, *head)))
            .filter(|head| form.section_tail(item, *head) == slot),
    }?;
    // A section the filter emptied is not drawn at all — heading, markers and everything. A
    // control that folds nothing away is worse than no control.
    if form.section_emptied(item, head) {
        return None;
    }
    Some(Focus::Section { item, slot: head, foot })
}

/// The fold row at `slot` of `item` the cursor may rest on: the section's TITLE — `>` shut, `v`
/// open — and never its `^` closing line, which is drawn but skipped.
///
/// Both ends used to be stops, and every walk down the form paid two keystrokes per section for
/// rows that only ever did one thing. The closing line lost its stop for good: Tab folds a section
/// from inside it, so the `^` had no job left. The title briefly lost its stop too, and got it
/// back: a section whose every entry is disabled or greyed has nothing inside for the cursor to
/// rest on, and with no title to land on either it could be opened and never folded again. One
/// stop per section is the price of never stranding a block open.
fn _fold_stop(form: &Form, item: usize, slot: usize) -> Option<Focus> {
    _folds(form, item, slot, false)
}

/// The focusable rows of `form`, in display order, always ending with Submit.
pub(crate) fn focusables(form: &Form) -> Vec<Focus> {
    let mut rows: Vec<Focus> =
        (0..form.filter_boxes().count()).map(|at| Focus::Filter { at }).collect();
    for (index, item) in form.items.iter().enumerate() {
        match item {
            Item::Comment(_) => {}
            // A disabled option gets no row, which IS the "not selectable" guarantee — the same
            // way a comment's absence here is what makes comments unreachable. Nothing downstream
            // needs to check: no key can name a row that does not exist. A folded option is
            // absent for the same reason, and the cursor cannot land on what is not drawn.
            Item::Checkboxes { options, .. } | Item::Radio { options, .. } => {
                for (option, entry) in options.iter().enumerate() {
                    rows.extend(_fold_stop(form, index, option));
                    // Greyed by an incompatibility gets no row either, and for the same reason
                    // a disabled one does not: the cursor cannot reach what does not exist here.
                    if entry.enabled
                        && !form.hidden(index, option)
                        && form.incompatible_at(index, option).is_none()
                    {
                        rows.push(Focus::Option { item: index, option });
                    }
                }
            }
            Item::Text { .. } => rows.push(Focus::Text { item: index }),
            // Row-major, so the flat order reads the way the table does — and so stepping one
            // place left or right lands in the neighbouring live column of the same row.
            //
            // Only cells that can be CHANGED get a row. A locked box is skipped: it is drawn so
            // a reader can see the choice exists, not so the cursor has to walk past it — with
            // ten columns and two of them live, stopping on the other eight is eight keystrokes
            // to reach nothing.
            Item::Grid { rows: grid, .. } => {
                for (row, entry) in grid.iter().enumerate() {
                    rows.extend(_fold_stop(form, index, row));
                    if !form.hidden(index, row) && form.incompatible_at(index, row).is_none() {
                        rows.extend(
                            entry.cells.iter().enumerate().filter(|(_, cell)| cell.enabled).map(
                                |(column, _)| Focus::Cell { item: index, row, column },
                            ),
                        );
                    }
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
/// - ↑/↓ move focus over the interactive rows, wrapping; comments are never visited.
/// - Space toggles a checkbox or picks a radio — except in a text field, where it types.
/// - Printable keys type into the focused text field; Backspace erases.
/// - Enter confirms Submit, and elsewhere hops to the next row (fill, Enter, fill, Enter…).
/// - Ctrl+A ticks every checkbox in the form; when all are already ticked, it unticks them
///   all. One key for both directions, because a classic terminal sends the identical byte
///   for Ctrl+A and Ctrl+Shift+A — they cannot be told apart. (`console` maps that byte to
///   `Key::Home`, so the physical Home key does the same, harmlessly.) Radios and text fields
///   are untouched: "all" is only a meaningful answer for choose-many.
/// - Ctrl+S submits from anywhere — unless an objection stands, in which case it does nothing,
///   exactly as Enter on a blocked Submit does. Arrives as [`CTRL_S`]: `console` leaves control
///   bytes it has no name for as `Key::Char`, and raw mode has cleared the flow-control meaning
///   the byte would otherwise have.
/// - Tab folds the section the cursor is in, and unfolds a shut one from its `>`. Folding lands
///   the cursor on that `>`; opening lands it on the first entry inside, or on the title when
///   nothing inside can be selected. A section's title is a stop whether open or shut; its `^`
///   closing line is drawn and skipped — with Tab folding from inside, a stop there had no job.
///   Tab does nothing outside a section, or in a form that does not fold. It used to be another
///   ↓; a grid already walks rows with ↓, and folding needed a key that works from INSIDE a block.
/// - Esc cancels.
pub(crate) fn apply(form: &mut Form, rows: &[Focus], focus: &mut usize, key: Key) -> Action {
    let editing_text = matches!(rows[*focus], Focus::Text { .. });
    match key {
        Key::Escape => return Action::Cancel,
        // Ctrl+A on a classic terminal (and the Home key). Toggle: all on, unless already all
        // on — then all off. Only checkbox groups; there is no "all" for radios or text.
        Key::Home => {
            // Only the boxes that are the user's to change: "all" cannot mean reaching past a
            // disabled or incompatible one, and neither may decide whether "all" is already
            // true. The two locks are checked together for the same reason `focusables` skips
            // both — a box the cursor cannot reach must not move when every box is asked to.
            //
            // Gathered up front, because the greying is a question ON the form and the second
            // pass needs the form mutably: the borrow rules say look, then touch.
            let yours = |form: &Form| {
                let mut live = Vec::new();
                for (index, item) in form.items.iter().enumerate() {
                    if let Item::Checkboxes { options, .. } = item {
                        for (option, entry) in options.iter().enumerate() {
                            if entry.enabled && form.incompatible_at(index, option).is_none() {
                                live.push((index, option, entry.checked));
                            }
                        }
                    }
                }
                live
            };
            let live = yours(form);
            if live.is_empty() {
                return Action::Ignored;
            }
            let everything_on = live.iter().all(|(_, _, checked)| *checked);
            for (index, option, _) in live {
                if let Item::Checkboxes { options, .. } = &mut form.items[index] {
                    options[option].checked = !everything_on;
                }
            }
        }
        // In a grid the arrows mean what they look like: sideways moves along a row, up and
        // down moves between rows keeping as near the same column as that row allows. Outside
        // one, and when a grid has no further row that way, they fall back to walking the flat
        // list — which is how the cursor gets out of a table and down to Submit.
        // On a fold row the arrows do what they do in every tree: left shuts, right opens. They
        // are free to — sideways movement means nothing on a row that is not in a grid — and it
        // spares the one-key ambiguity of space having to guess which way you meant. Opening
        // lands the cursor inside the section (see `_refocus_fold`); shutting leaves it on the
        // `>`. Tab does the same from inside a section, where its title is out of reach.
        Key::ArrowLeft | Key::ArrowRight if matches!(rows[*focus], Focus::Section { .. }) => {
            let Focus::Section { item, slot, .. } = rows[*focus] else { unreachable!() };
            let shut = key == Key::ArrowLeft;
            let changed = match shut {
                true => form.collapsed.insert((item, slot)),
                false => form.collapsed.remove(&(item, slot)),
            };
            if !changed {
                return Action::Ignored; // already that way
            }
            _refocus_fold(form, focus, item, slot, shut);
        }
        Key::ArrowLeft | Key::ArrowRight => {
            let forward = key == Key::ArrowRight;
            match _along_row(rows, *focus, forward) {
                Some(next) => *focus = next,
                None => return Action::Ignored,
            }
        }
        Key::ArrowDown => {
            *focus = _across_rows(rows, *focus, true)
                .unwrap_or_else(|| (*focus + 1) % rows.len());
        }
        Key::Tab => {
            if !form.collapsible {
                return Action::Ignored;
            }
            let (item, slot) = match rows[*focus] {
                Focus::Section { item, slot, .. } => (item, slot),
                Focus::Option { item, option: slot } | Focus::Cell { item, row: slot, .. } => {
                    match form.section_head(item, slot) {
                        Some(head) => (item, head),
                        None => return Action::Ignored, // loose rows above the first heading
                    }
                }
                Focus::Filter { .. } | Focus::Text { .. } | Focus::Submit => return Action::Ignored,
            };
            let shut = !form.collapsed.remove(&(item, slot));
            if shut {
                form.collapsed.insert((item, slot));
            }
            _refocus_fold(form, focus, item, slot, shut);
        }
        Key::ArrowUp => {
            *focus = _across_rows(rows, *focus, false)
                .unwrap_or_else(|| (*focus + rows.len() - 1) % rows.len());
        }
        // Before the general `Char` arm below, which would otherwise type this into a text field.
        Key::Char(CTRL_S) => {
            return match form.objections().is_empty() {
                true => Action::Submit,
                false => Action::Ignored,
            };
        }
        Key::Enter => match rows[*focus] {
            // Refused rather than submitted, and NOT silently: the objections are already drawn
            // under the button, so the press that does nothing is the press that points at them.
            Focus::Submit if !form.objections().is_empty() => return Action::Ignored,
            Focus::Submit => return Action::Submit,
            _ => *focus = (*focus + 1) % rows.len(),
        },
        Key::Char(' ') if !editing_text => {
            // A filter box ticks and unticks like any other — what differs is that it changes
            // which OTHER rows exist, so the cursor is re-found afterwards by identity.
            if let Focus::Filter { at } = rows[*focus] {
                let Some(rule) = form.filter_boxes().nth(at) else { return Action::Ignored };
                let label = rule.label.clone();
                if !form.excluded.remove(&label) {
                    form.excluded.insert(label);
                }
                let here = Focus::Filter { at };
                if let Some(now) = focusables(form).iter().position(|row| *row == here) {
                    *focus = now;
                }
                return Action::Redraw;
            }
            // A fold row has nothing to tick, so space means the only thing it could: flip it.
            if let Focus::Section { item, slot, .. } = rows[*focus] {
                let shut = form.collapsed.insert((item, slot));
                if !shut {
                    form.collapsed.remove(&(item, slot));
                }
                _refocus_fold(form, focus, item, slot, shut);
                return Action::Redraw;
            }
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
                    Item::Checkboxes { options, .. } => {
                        options[option].checked = !options[option].checked;
                        if form.mirror_duplicates {
                            mirror = Some((options[option].name.clone(), options[option].checked));
                        }
                    }
                    // A radio picks; it does not un-pick — that's what makes it a radio.
                    Item::Radio { chosen, .. } => *chosen = Some(option),
                    _ => {}
                }
                // The same wording elsewhere IS the same entry: its boxes follow this one, in
                // every group at once — a thing cannot be both doomed and spared.
                if let Some((name, state)) = mirror {
                    // A twin the user cannot touch is not moved by touching its sibling either —
                    // "not yours to change" holds from every angle, and a rule-greyed twin is as
                    // locked as a disabled one. (Which CAN leave twins disagreeing, exactly as
                    // disabled twins always could: the locked one keeps reporting the machine,
                    // the live one the answer. The contradiction to avoid was two ANSWERS.)
                    let locked: Vec<(usize, usize)> = form
                        .items
                        .iter()
                        .enumerate()
                        .flat_map(|(index, item)| match item {
                            Item::Checkboxes { options, .. } => (0..options.len())
                                .filter(|option| form.incompatible_at(index, *option).is_some())
                                .map(|option| (index, option))
                                .collect(),
                            _ => Vec::new(),
                        })
                        .collect();
                    for (index, item) in form.items.iter_mut().enumerate() {
                        if let Item::Checkboxes { options, .. } = item {
                            for (option, twin) in options.iter_mut().enumerate() {
                                let free = twin.enabled && !locked.contains(&(index, option));
                                if twin.name == name && free {
                                    twin.checked = state;
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

/// Whether a keystroke is ALREADY waiting to be read — asked, not waited on.
///
/// The same `poll` as [`_await_input`] with a zero timeout, which is the whole difference: that
/// one blocks until there is news, this one reports whether there is any right now.
fn _input_pending(fd: std::os::fd::RawFd) -> bool {
    let mut watch = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: as `_await_input` — `poll` touches only the pollfd on this stack frame. A zero
    // timeout cannot block.
    unsafe { libc::poll(&mut watch, 1, 0) > 0 && watch.revents & libc::POLLIN != 0 }
}

/// How many keystrokes may be folded into one frame before painting anyway.
///
/// Insurance rather than tuning. The drain ends on its own the moment the terminal has nothing
/// waiting, and reading a key is microseconds against milliseconds to paint — so a human, even
/// leaning on an arrow key, can never outrun it. What this bounds is the pathological case: a
/// paste, or a terminal replaying a long escape burst, where input arrives faster than any
/// reader. Without it, such a stream could hold the screen still indefinitely.
const COALESCE: usize = 128;

/// Whether to swallow the repaint this key earned, because more input is already waiting.
///
/// Pure, so the one rule that must never bend can be tested: SUBMIT AND CANCEL ARE NEVER
/// SWALLOWED. They end the run, and a run that ended is not waiting for a tidier moment.
fn _coalesce(action: &Action, pending: bool, drained: usize) -> bool {
    matches!(action, Action::Redraw) && pending && drained < COALESCE
}

/// Paint `lines` over the frame already on screen, which was `previous` lines tall.
///
/// One write, and no blank state in between. Both matter, and for different reasons.
///
/// ONE WRITE, because `Term::stderr()` is unbuffered: every `write_line`, every `clear_line`,
/// every cursor move is its own `write(2)`. The obvious repaint — clear the old block, print the
/// new one — costs `3n + 2` of them, which is 122 syscalls for a 40-line form and 242 for an
/// 80-line one. A terminal renders as those arrive, so the user watches the block blank line by
/// line and fill line by line. That is the flicker. A buffered [`Term`] plus one flush makes the
/// whole frame a single write, and the terminal has nothing to show halfway through.
///
/// NO BLANK STATE, because "erase n lines, then draw n lines" describes an empty screen even
/// when it arrives in one piece. Each line is instead overwritten where it stands and followed
/// by `\x1b[K`, which clears only whatever the old line left to the right of it. Nothing is ever
/// blank, so nothing can be caught blank.
///
/// The cursor starts and finishes immediately after the block, which is what lets the next call
/// find it by moving up `previous` lines.
pub(crate) fn _paint(term: &Term, lines: &[String], previous: usize) -> std::io::Result<()> {
    term.write_str(&_frame(lines, previous))?;
    term.flush()
}

/// The bytes one repaint sends, as a single string — pure, so the escape arithmetic can be
/// tested without a terminal like everything else that decides what appears.
///
/// Built whole before anything is written, which makes the single write a property of this
/// function rather than a hope about the buffer underneath it.
///
/// The cursor ends `lines.len()` rows below where the old block began — that is, immediately
/// after the new one — which is the invariant the next call depends on. An empty frame therefore
/// leaves it exactly where the block started, having erased the lot.
fn _frame(lines: &[String], previous: usize) -> String {
    let mut out = String::new();
    if previous > 0 {
        out.push_str(&format!("\x1b[{previous}A"));
    }
    for line in lines {
        out.push_str(line);
        // Erase only what the old line left to the RIGHT of this one. Nothing is ever blanked
        // first, so no repaint can be caught halfway.
        out.push_str("\x1b[K\n");
    }
    // A frame that SHRANK — a section folded, a warning stopped applying — leaves rows of the
    // old one below it. Wipe those, then come back to sit just under the new frame.
    let surplus = previous.saturating_sub(lines.len());
    for _ in 0..surplus {
        out.push_str("\x1b[K\n");
    }
    if surplus > 0 {
        out.push_str(&format!("\x1b[{surplus}A"));
    }
    out
}

/// Put the cursor somewhere sensible after a section was just folded or unfolded.
///
/// Folding removes rows and unfolding adds them, so the index the cursor held is about to name a
/// different row. It is re-found by IDENTITY rather than arithmetic, in the list as it now
/// stands: a section just SHUT leaves the cursor on its `>`, and one just OPENED puts it on the
/// first entry inside — or on its `v` when nothing inside can be selected, so that it can always
/// be folded again.
fn _refocus_fold(form: &Form, focus: &mut usize, item: usize, slot: usize, shut: bool) {
    let rows = focusables(form);
    let landing = match shut {
        // Shut: the `>` that now stands for the whole section — the one fold row that is a stop.
        true => rows.iter().position(|row| *row == Focus::Section { item, slot, foot: false }),
        // Opened: the first entry inside, because the point of opening a section is to get at
        // what it holds, and landing on its name would be one more key before anything could be
        // done. A section with nothing selectable inside falls back to its title — the one row
        // that is always there, and the one that can fold it again.
        false => {
            let tail = form.section_tail(item, slot);
            let title = Focus::Section { item, slot, foot: false };
            rows.iter()
                .position(|row| match row {
                    Focus::Option { item: i, option: s } | Focus::Cell { item: i, row: s, .. } => {
                        *i == item && (slot..=tail).contains(s)
                    }
                    _ => false,
                })
                .or_else(|| rows.iter().position(|row| *row == title))
        }
    };
    if let Some(at) = landing {
        *focus = at;
    }
}

/// The next focusable cell along the SAME grid row, in the given direction — `None` when the
/// cursor is not in a grid, or has run out of row.
///
/// Sideways movement never leaves the row it started in. Wrapping to the next row would make a
/// grid behave like a flat list that happens to be drawn in a table, which is the one thing a
/// table is not.
fn _along_row(rows: &[Focus], focus: usize, forward: bool) -> Option<usize> {
    let Focus::Cell { item, row, .. } = rows[focus] else { return None };
    _walk(rows, focus, forward)
        .take_while(|at| matches!(rows[*at], Focus::Cell { item: i, row: r, .. } if i == item && r == row))
        .next()
}

/// The focus indices from `focus` outward, in one direction, not including `focus` itself.
fn _walk(rows: &[Focus], focus: usize, forward: bool) -> Box<dyn Iterator<Item = usize>> {
    match forward {
        true => Box::new(focus + 1..rows.len()),
        false => Box::new((0..focus).rev()),
    }
}

/// The focusable cell nearest the current column, one grid row up or down — `None` when the
/// cursor is not in a grid, or the grid has no further row that way.
///
/// Rows with nothing live are stepped over rather than stopping the cursor, and the landing
/// column is the nearest CHANGEABLE cell to where the cursor already was: a table that threw the
/// cursor back to column one on every vertical move would be unusable with ten columns.
fn _across_rows(rows: &[Focus], focus: usize, down: bool) -> Option<usize> {
    let Focus::Cell { item, row, column } = rows[focus] else { return None };
    // Walked over the FOCUS LIST, not over the grid's rows. The list already leaves out what is
    // folded, filtered or greyed, so nothing here has to ask twice — and it contains the fold
    // rows, which a walk over grid rows never saw. That was the bug: `v` and `^` sat between two
    // rows of cells and the cursor stepped straight over them, so nothing in a grid could fold.
    for at in _walk(rows, focus, down) {
        match rows[at] {
            // Still the row we left: its other cells, on the way past.
            Focus::Cell { item: i, row: r, .. } if i == item && r == row => continue,
            // A fold row of this grid is a stop in its own right.
            Focus::Section { item: i, .. } if i == item => return Some(at),
            // The first cell of another row of this grid: land in THAT row, in the column nearest
            // the one we came from, so a column read down stays a column.
            Focus::Cell { item: i, row: r, .. } if i == item => {
                return rows
                    .iter()
                    .enumerate()
                    .filter_map(|(at, here)| match here {
                        Focus::Cell { item: ii, row: rr, column: c } if *ii == i && *rr == r => {
                            Some((at, *c))
                        }
                        _ => None,
                    })
                    .min_by_key(|(_, c)| (c.abs_diff(column), *c))
                    .map(|(at, _)| at);
            }
            // Out of the grid — another item, a filter box, Submit: the flat list takes over.
            _ => return None,
        }
    }
    None
}

/// The input the form reads: stdin when it's a terminal, `/dev/tty` otherwise — the same
/// choice `console` makes internally, so the fd we wait on and configure is the fd it reads.
/// The `File` half keeps a non-stdin tty open for as long as the handle lives.
pub(crate) fn _input_fd() -> std::io::Result<(std::os::fd::RawFd, Option<std::fs::File>)> {
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
pub(crate) struct RawMode {
    fd: std::os::fd::RawFd,
    original: libc::termios,
}

impl RawMode {
    pub(crate) fn engage(fd: std::os::fd::RawFd) -> std::io::Result<Self> {
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
pub(crate) fn _await_input(fd: std::os::fd::RawFd) -> std::io::Result<bool> {
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
/// [`compose`]'s lines alone — what the drawing tests read, since none of them cares where the
/// footer starts. The run loop uses [`compose`] itself.
#[cfg(test)]
pub(crate) fn render(
    form: &Form,
    rows: &[Focus],
    focus: usize,
    width: usize,
    warnings: &[String],
    opened: &Opened,
) -> Vec<String> {
    compose(form, rows, focus, width, warnings, opened).lines
}

/// A drawn form, with the two facts a viewport needs that the lines alone do not say.
pub(crate) struct Frame {
    pub lines: Vec<String>,
    /// The line the cursor is on — the one carrying [`FOCUS_ON`].
    pub focused: usize,
    /// Where the footer begins: the blank above Submit, then Submit, the preview, the objections,
    /// the cautions and the key hints. Everything from here down is about the WHOLE answer or
    /// about the row under the cursor, and stays on screen however far the body scrolls.
    pub foot: usize,
}

/// The reverse-video that marks the focused row — the one escape every emitter of focus uses,
/// and the one thing [`compose`] looks for to know which line the cursor is on. Raw rather than a
/// `console` style, so it is present under `NO_COLOR` too: the cursor is not decoration.
const FOCUS_ON: &str = "\x1b[7m";

/// The whole form as displayable lines, the focused row inverted, everything clipped to `width`
/// so no line can wrap (a wrapped line would break the redraw arithmetic — the loop clears
/// exactly as many lines as it printed) — together with where the cursor and the footer are, so
/// a viewport can choose which of the lines to show. See [`Frame`].
pub(crate) fn compose(
    form: &Form,
    rows: &[Focus],
    focus: usize,
    width: usize,
    warnings: &[String],
    opened: &Opened,
) -> Frame {
    // What is demanded and unsupplied, asked ONCE — `wanted_among` per row against this, where
    // `wanted_at` per row would recompute the whole walk and turn the repaint quadratic.
    let unmet = form.unmet();
    let mut lines = Vec::new();
    if let Some(title) = &form.title {
        lines.push(console::style(title).bold().to_string());
        lines.push(String::new());
    }
    // The filter block, above everything it governs. Drawn here rather than as an `Item` on
    // purpose: it is not an answer, and an item would put it in `answers_toml` alongside the
    // things the user was actually asked.
    if form.filter_boxes().next().is_some() {
        let violet = |text: String| console::style(text).color256(FILTER_VIOLET).to_string();
        if !form.filter_label.is_empty() {
            lines.push(violet(format!("{}:", form.filter_label)));
        }
        // Each box says how many entries it governs, and the counts stand in a column of their
        // own: labels padded to the widest, counts right-aligned to the widest. A ragged column
        // of numbers is harder to compare than no numbers.
        let boxes: Vec<_> = form.filter_boxes().map(|rule| (rule, form.governed(rule))).collect();
        let label_width =
            boxes.iter().map(|(rule, _)| console::measure_text_width(&rule.label)).max().unwrap_or(0);
        let count_width = boxes.iter().map(|(_, count)| count.to_string().len()).max().unwrap_or(1);
        for (at, (rule, count)) in boxes.iter().enumerate() {
            let box_mark = if form.excluded.contains(&rule.label) { "[ ]" } else { "[x]" };
            let pad = " ".repeat(label_width - console::measure_text_width(&rule.label));
            // Coloured BEFORE the focus mark, so the re-arming below carries the violet's own
            // reset through the reverse video instead of being cut short by it — the same order
            // the cleared-box red is applied in.
            let row = violet(format!("{box_mark} {}{pad} ({count:>count_width$})", rule.label));
            lines.push(match rows.get(focus) == Some(&Focus::Filter { at }) {
                true => format!("\x1b[7m▸ {}\x1b[0m", row.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
                false => format!("  {row}"),
            });
        }
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
        true => format!("{FOCUS_ON}▸ {}\x1b[0m", line.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
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
            Item::Checkboxes { label, options } => {
                // An anonymous group draws no heading — it exists to sit flush inside
                // surrounding comments (a tree of rows, some of them tickable).
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, entry) in options.iter().enumerate() {
                    match form.collapsible {
                        true => lines.extend(_fold_lines(form, index, option, false, rows, focus, &clean)),
                        false => lines.extend(subtitle(&entry.heading, comment_indent, &clean)),
                    }
                    if form.hidden(index, option) {
                        lines.extend(_fold_lines(form, index, option, true, rows, focus, &clean));
                        continue;
                    }
                    let box_mark = _box(entry.checked, opened.ticked(index, 0, option));
                    let row = format!("{box_mark} {}", clean(entry.name.clone()));
                    // Colour BEFORE the focus mark, so that `mark`'s re-arming carries its reset
                    // through the reverse video rather than being cut short by it.
                    //
                    // The two can never both apply: red means a tick was cleared, blue means one
                    // is still standing, and `Opened` records a suggestion as clear so a cleared
                    // suggestion is not red either.
                    let hinted = entry.checked && entry.suggested;
                    let row = match (
                        opened.cleared(index, 0, option, entry.checked),
                        hinted,
                        form.wanted_among(&unmet, index, option),
                    ) {
                        (true, _, _) => console::style(row).red().to_string(),
                        (_, true, _) => console::style(row).color256(SUGGESTED_BLUE).to_string(),
                        // Green last of the three: red and blue are about THIS entry's own tick,
                        // and green is about a hole somewhere else that this entry could fill.
                        // An entry that is both is better described by its own state.
                        (_, _, true) => console::style(row).color256(WANTED_GREEN).to_string(),
                        _ => row,
                    };
                    lines.push(match entry.enabled && form.incompatible_at(index, option).is_none() {
                        // Dim, and never marked: the focus list has no row for it, so `mark`
                        // could not report it focused anyway — this only says so visibly.
                        false => console::style(format!("  {row}")).dim().to_string(),
                        true => mark(row, Focus::Option { item: index, option }),
                    });
                    lines.extend(_fold_lines(form, index, option, true, rows, focus, &clean));
                }
            }
            Item::Radio { label, options, chosen } => {
                if !label.is_empty() {
                    lines.push(format!("{label}:"));
                }
                for (option, entry) in options.iter().enumerate() {
                    match form.collapsible {
                        true => lines.extend(_fold_lines(form, index, option, false, rows, focus, &clean)),
                        false => lines.extend(subtitle(&entry.heading, comment_indent, &clean)),
                    }
                    if form.hidden(index, option) {
                        lines.extend(_fold_lines(form, index, option, true, rows, focus, &clean));
                        continue;
                    }
                    let dot = if *chosen == Some(option) { "(•)" } else { "( )" };
                    let row = format!("{dot} {}", clean(entry.name.clone()));
                    let row = match form.wanted_among(&unmet, index, option) {
                        true => console::style(row).color256(WANTED_GREEN).to_string(),
                        false => row,
                    };
                    lines.push(match entry.enabled && form.incompatible_at(index, option).is_none() {
                        false => console::style(format!("  {row}")).dim().to_string(),
                        true => mark(row, Focus::Option { item: index, option }),
                    });
                    lines.extend(_fold_lines(form, index, option, true, rows, focus, &clean));
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
                //
                // Aligned by hand rather than by `table_formatter`, which is the sibling crate
                // that exists for exactly this and is what bashrs uses for every other table.
                // Measured before deciding: it would take this crate from 15 dependencies to 45
                // — clap, rayon, regex, syn — to replace the six lines below, and `console` is
                // already here and already counts display width with ANSI and wide glyphs
                // handled, which is the only hard part.
                //
                // The shapes also fit worse than they look. That crate aligns text it splits on
                // two-space runs; a grid has fixed-width boxes and varying headings, so cells
                // would have to be serialised into a delimited string, re-measured, and parsed
                // back — with the focus reverse-video and the red deviation marks riding inside
                // them. Two of its defaults would need turning off besides (`reasonable-spacing`
                // must never drop a grid row, and the trailing pad is deliberate so the `#`
                // notes line up).
                //
                // Revisit if this crate ever gains `table_formatter` for another reason: the
                // objection is the dependency, not the idea.
                let pad = |text: &str, width: usize| {
                    " ".repeat(width.saturating_sub(console::measure_text_width(text)))
                };
                let slots: Vec<usize> = columns
                    .iter()
                    .map(|column| console::measure_text_width(column).max(BOX) + GAP)
                    .collect();
                // A grid's rows carry no row-level focus marker — the reverse video goes on the
                // cell — so they normally start flush. A folding grid has fold rows among them
                // that DO need that two-column marker slot, so the whole table steps right to
                // meet them, header included. Uniform, and the columns stay square.
                //
                // Deliberately a step and not a NEST: the mockup indents a section's contents
                // under its title, which a grid cannot do without moving its boxes out from
                // under its column headings. The `v`/`^` pair is what delimits a section here,
                // which is the job that pair was invented for.
                let lead = if form.collapsible { "  " } else { "" };
                let heads: String = columns
                    .iter()
                    .zip(&slots)
                    .map(|(column, slot)| format!("{column}{}", pad(column, *slot)))
                    .collect();
                let header = console::style(format!("{lead}{heads}")).dim().to_string();
                // A folding grid repeats the header under every open title (below), so its own
                // copy at the top would head nothing but the first title — dropped, unless loose
                // rows come before that title and have no copy to read from. A grid that does not
                // fold gets no copies, and keeps the one header it always had.
                let titled_from_the_top =
                    form.collapsible && grid.first().is_some_and(|row| row.heading.is_some());
                if !titled_from_the_top {
                    lines.push(header.clone());
                }

                // Labels align into a column, so the notes after them do too — a ragged right
                // edge of `#` remarks is harder to read past than no remarks at all.
                let widest = grid
                    .iter()
                    .map(|row| console::measure_text_width(&row.label))
                    .max()
                    .unwrap_or(0);
                for (row, entry) in grid.iter().enumerate() {
                    let title = match form.collapsible {
                        true => _fold_lines(form, index, row, false, rows, focus, &clean),
                        false => subtitle(&entry.heading, "", &clean),
                    };
                    // In a folding grid every open block repeats the column headers under its
                    // title. A table long enough to scroll is one where the headers at the top left
                    // the screen rows ago, and a box in the fourth column then says nothing about
                    // WHICH way of having the thing it is. A shut block has no boxes to head. The
                    // copy is the same dim line, and no more selectable than the original. Plain
                    // grids get none: their one header at the top stays, and nothing repeats it.
                    let opens_a_block = form.collapsible
                        && !title.is_empty()
                        && !form.collapsed.contains(&(index, row));
                    lines.extend(title);
                    if opens_a_block {
                        lines.push(header.clone());
                    }
                    if form.hidden(index, row) {
                        lines.extend(_fold_lines(form, index, row, true, rows, focus, &clean));
                        continue;
                    }
                    // One question per row rather than per cell: an entry this machine cannot
                    // run is out of reach through EVERY manager, so the whole row locks together.
                    let greyed = form.incompatible_at(index, row).is_some();
                    let boxes: String = entry
                        .cells
                        .iter()
                        .enumerate()
                        // A row may carry fewer cells than there are columns — that is documented,
                        // and the rest draw blank. It may also carry MORE, which nothing forbids
                        // and which used to index `slots` off the end and panic mid-render.
                        // There is no column to draw them in, so they are dropped, exactly as
                        // `answers_toml` already drops them.
                        .take(columns.len())
                        .map(|(column, cell)| {
                            let drawn = _box(cell.checked, opened.ticked(index, row, column));
                            // Padding is added AFTER any styling, so an escape never counts
                            // towards the width and the columns stay straight.
                            let inked = match (cell.boxed, cell.enabled && !greyed) {
                                // Nothing this column could ever do for this row.
                                (false, _) => console::style(" · ").dim().to_string(),
                                // A choice that exists but is out of reach — shown as the box it
                                // is, so a reader can see what setting something up would unlock,
                                // and dark enough that nobody mistakes it for one they can pick.
                                (true, false) => {
                                    console::style(drawn).color256(LOCKED_GREY).to_string()
                                }
                                (true, true) => {
                                    match (
                                        opened.cleared(index, row, column, cell.checked),
                                        cell.checked && cell.suggested,
                                    ) {
                                        (true, _) => console::style(drawn).red().to_string(),
                                        (_, true) => {
                                            console::style(drawn).color256(SUGGESTED_BLUE).to_string()
                                        }
                                        _ => drawn.to_string(),
                                    }
                                }
                            };
                            let inked = match rows[focus] == (Focus::Cell { item: index, row, column }) {
                                true => format!("\x1b[7m{}\x1b[0m", inked.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
                                false => inked,
                            };
                            format!("{inked}{}", pad(drawn, slots[column]))
                        })
                        .collect();
                    let named = clean(entry.label.clone());
                    let plain = named.clone();
                    let named = match (greyed, form.wanted_among(&unmet, index, row)) {
                        (true, _) => console::style(named).dim().to_string(),
                        (_, true) => console::style(named).color256(WANTED_GREEN).to_string(),
                        _ => named,
                    };
                    let note = entry.note.as_ref().map_or(String::new(), |note| {
                        // Measured against the UNSTYLED label: an escape sequence has no width
                        // on screen, and counting one would push every note after it out of line.
                        let gap = pad(&plain, widest);
                        console::style(format!("{gap}  # {}", clean(note.clone()))).dim().to_string()
                    });
                    lines.push(format!("{lead}{boxes}{named}{note}"));
                    lines.extend(_fold_lines(form, index, row, true, rows, focus, &clean));
                }
            }
        }
    }
    let foot = lines.len();
    lines.push(String::new());
    // A blocked Submit is still focusable and still drawn as a button — pressing it is how a
    // user asks what is wrong, and the objections are already on screen under it. Hiding the
    // button would leave them no way to find out and nothing to aim at.
    let objections = form.objections();
    lines.push(match objections.is_empty() {
        true => mark("[ Submit ]".to_string(), Focus::Submit),
        false => mark(console::style("[ Submit ]").dim().to_string(), Focus::Submit),
    });
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
    // Objections before cautions, because they are the stronger claim: a caution says "look at
    // this before you go", an objection says "you cannot go". A different glyph for the same
    // reason — two red lines that read alike would blur into one severity.
    for objection in &objections {
        for (line, text) in _wrap(&clean(objection.clone()), width.saturating_sub(4)).iter().enumerate()
        {
            let lead = if line == 0 { "  \u{2717} " } else { "    " };
            lines.push(console::style(format!("{lead}{text}")).red().bold().to_string());
        }
    }
    for warning in warnings {
        for (line, text) in _wrap(&clean(warning.clone()), width.saturating_sub(4)).iter().enumerate()
        {
            // Continuations hang under the first line's text, not under its marker.
            let lead = if line == 0 { "  ⚠ " } else { "    " };
            lines.push(console::style(format!("{lead}{text}")).red().to_string());
        }
    }
    // Only the keys this form answers to: a fold key on a form with nothing to fold would be a
    // promise the form cannot keep.
    let fold = if form.collapsible { " · tab fold/unfold" } else { "" };
    lines.push(
        console::style(format!(
            "↑/↓/←/→ move · space picks · ctrl+a all/none · ctrl+s submit{fold} · enter next/submit · esc cancels"
        ))
        .dim()
        .to_string(),
    );
    // Found by its own marker rather than tracked through every push, because three places emit
    // focus (a row, a fold title, a grid cell) and one search cannot disagree with itself. It is
    // this crate's escape it is looking for, not a reading of arbitrary text: content is cleaned
    // of colour when `scrub_colors` is on, and a caller who paints reverse video into an option
    // has chosen to look like the cursor.
    let focused = lines.iter().position(|line| line.contains(FOCUS_ON)).unwrap_or(0);
    let lines = lines.into_iter().map(|line| console::truncate_str(&line, width, "…").into_owned()).collect();
    Frame { lines, focused, foot }
}

/// The part of `frame` that fits in `height` lines, with the focused line among them.
///
/// The footer is PINNED — Submit, the preview of what the cell under the cursor would run, the
/// objections, the cautions, the key hints — and the body above it scrolls. The preview is the
/// reason: it is about the row the cursor is on, and a form tall enough to scroll is exactly the
/// one where that row is nowhere near the bottom. Pinning it is what keeps "what would this do"
/// answerable from row 40 of 600.
///
/// `scroll` is the body's first visible line as it was, and comes back adjusted: moved only as far
/// as it must to bring the focused line into view, so the picture stays still while the cursor
/// moves within it and slides one row at a time when the cursor reaches an edge. Focus in the
/// footer leaves the body where it was.
///
/// A terminal too short to hold the footer and three lines of body gives up pinning and scrolls
/// the whole frame as one — a footer that ate the screen would leave no room for the question.
pub(crate) fn viewport(frame: &Frame, height: usize, scroll: usize) -> (Vec<String>, usize) {
    let total = frame.lines.len();
    if total <= height {
        return (frame.lines.clone(), 0);
    }
    let foot_len = total - frame.foot;
    let body_height = height.saturating_sub(foot_len);
    let (window, cut) = match body_height >= MIN_BODY {
        true => (body_height, frame.foot),
        false => (height, total),
    };
    let scroll = scroll.min(cut.saturating_sub(window));
    let scroll = match frame.focused {
        at if at >= cut => scroll,
        at if at < scroll => at,
        at if at >= scroll + window => at + 1 - window,
        _ => scroll,
    };
    let mut shown: Vec<String> = frame.lines[scroll..(scroll + window).min(cut)].to_vec();
    if cut == frame.foot {
        shown.extend_from_slice(&frame.lines[frame.foot..]);
    }
    (shown, scroll)
}

/// The fewest body lines worth pinning a footer over. Below this the footer is the screen, and
/// the question it is a footer TO would be invisible.
const MIN_BODY: usize = 3;

/// The marker a foldable section draws: `>` shut, `v` open, `^` closing an open one.
///
/// Arrows rather than a box, because these are not answers — a reader glancing down a form should
/// never have to work out whether `[+]` is something they ticked.
const SHUT: char = '>';
const OPEN: char = 'v';
const CLOSE: char = '^';

/// The fold row before or after slot `slot`, drawn — or nothing, which is the usual answer.
///
/// Mirrors [`_folds`] exactly, and must: a marker drawn where no focus row exists could never be
/// pressed, and a focus row with no marker would be an invisible stop for the cursor. The two are
/// kept in step by asking the same question, so there is one definition of where a fold is.
///
/// Not dim, unlike an ordinary sub-title. A dim row that the cursor lands on reads as a mistake,
/// and these are the only headings that are also controls.
#[allow(clippy::too_many_arguments)]
fn _fold_lines(
    form: &Form,
    item: usize,
    slot: usize,
    foot: bool,
    rows: &[Focus],
    focus: usize,
    clean: &impl Fn(String) -> String,
) -> Vec<String> {
    let Some(here @ Focus::Section { slot: head, .. }) = _folds(form, item, slot, foot) else {
        return Vec::new();
    };
    let Some(text) = form.heading_at(item, head) else { return Vec::new() };
    let shut = form.collapsed.contains(&(item, head));
    let arrow = match (foot, shut) {
        (true, _) => CLOSE,
        (false, true) => SHUT,
        (false, false) => OPEN,
    };
    // A multi-line heading marks only its first line; the rest are continuation, and four arrows
    // down the left of one title would read as four sections.
    //
    // The FOOT takes that first line and nothing else. It exists to say which section just ended,
    // and a two-line title restated under its own options is noise where a name would do.
    let said = clean(text.to_string());
    let body: Vec<&str> = match foot {
        true => said.lines().take(1).collect(),
        false => said.lines().collect(),
    };
    body.into_iter()
        .enumerate()
        .map(|(line, said)| {
            let lead = match line {
                0 => format!("{arrow} "),
                _ => "  ".to_string(),
            };
            // The filter block's violet, because a fold title does the filter block's job: both
            // decide what is on screen and neither is an answer. One colour for one kind of
            // control, and a reader learns it once. The FOOT takes a darker shade of it: a `^`
            // in the same violet as the `v` beneath it read as the next section beginning.
            let shade = if foot { FOLD_FOOT_VIOLET } else { FILTER_VIOLET };
            let text = console::style(format!("{lead}{said}")).color256(shade).to_string();
            match rows.get(focus) == Some(&here) && line == 0 {
                true => format!("{FOCUS_ON}▸ {}\x1b[0m", text.replace("\x1b[0m", "\x1b[0m\x1b[7m")),
                false => format!("  {text}"),
            }
        })
        .collect()
}

/// A sub-title standing above option `slot`, as rendered lines — dim, like a comment, because
/// that is what it is: the file's own words about the options beneath it. Empty when the option
/// carries none, which is most of them.
fn subtitle(
    heading: &Option<String>,
    indent: &str,
    clean: &impl Fn(String) -> String,
) -> Vec<String> {
    let Some(said) = heading else {
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

/// The grey a locked box is drawn in — 256-colour, deliberately darker than `dim`.
///
/// `dim` is whatever a terminal decides it is, and on a dark theme it lands close enough to the
/// live boxes that a whole column of them still reads as pickable. A fixed dark grey says
/// switched-off at a glance, which is the entire job: these are drawn so a reader can see the
/// choice exists, and confusing them for live ones defeats that.
const LOCKED_GREY: u8 = 240;

/// The violet the filter block is drawn in — 256-colour, like [`LOCKED_GREY`] and for the same
/// reason: a fixed colour rather than whatever a theme decides an attribute means.
///
/// It earns a colour of its own because it is the one block on screen that is NOT part of the
/// answers. Everything else a user ticks is a reply to a question; these boxes only decide what
/// they can see. Red is spoken for twice over (cautions, and a box the form arrived with that
/// has been cleared) and grey already means switched-off, so violet is what was left unclaimed.
const FILTER_VIOLET: u8 = 141;

/// The `^` closing an open section: the same hue as [`FILTER_VIOLET`], two steps darker on the
/// 256-colour cube, so the line that ENDS a section is not mistaken for the one that begins the
/// next. Darker rather than `dim`, because terminals disagree about what faint does to a
/// 256-colour foreground and agree about what colour 97 is.
const FOLD_FOOT_VIOLET: u8 = 97;

/// The blue a suggested tick is drawn in — 256-colour, as above.
///
/// Blue says "the form put this here", where an uncoloured tick says "this is already so" and a
/// red one says "you have undone something". Three states, three colours, and the middle one is
/// the only one that is a recommendation rather than a report.
const SUGGESTED_BLUE: u8 = 39;

/// A box ticked by the MACHINE — installed already, or otherwise so when the form opened. A filled
/// square, and the point is that it is not an `x`: nobody put it there, so a reader can tell the
/// form's facts from their own choices at a glance. Clearing it undoes a fact and is marked red;
/// ticking it again brings the square back rather than an `x`, because what the square says is
/// "as it was when we started", and that is true again.
///
/// The glyph took four tries, and the reasons are worth keeping. The full block `█` IS the
/// cursor's glyph, and under the reverse-video focus it inverted into a solid dark cell — the one
/// box that most needed to read as ticked looked like a hole. The vertical rectangle `▮` stood the
/// brackets' full height and read as a bar, not a mark. The medium square `◼` LOOKED best — most
/// fonts draw it a little smaller and centred higher, between the brackets' ends — and lost anyway,
/// on safety: Unicode lists it as emoji-capable, and a terminal that forces an emoji font on it
/// draws it two cells wide and breaks every column. `■` has no emoji property at all, so it is one
/// cell in every terminal that agrees with itself about width. It sits low in most monospace fonts,
/// level with the brackets' feet, and that is the price of the guarantee.
///
/// Where a glyph falls is the font's decision, not Unicode's: `printf '[■] [◼] [▪]\n'` in the
/// terminal the form will be read in is the whole test, should the trade ever be revisited. Width
/// is ambiguous — one cell outside CJK locales, like the `▸` cursor marker — and the test below
/// holds it at one.
const GIVEN: &str = "[■]";
/// A box the user ticked — or one the form suggested, which is a tick that asserts nothing and is
/// told apart by its colour rather than its glyph.
const TICKED: &str = "[x]";
const CLEAR: &str = "[ ]";

/// The glyph for a box, from whether it is ticked now and whether it arrived that way.
const fn _box(checked: bool, given: bool) -> &'static str {
    match (checked, given) {
        (true, true) => GIVEN,
        (true, false) => TICKED,
        (false, _) => CLEAR,
    }
}

/// What ctrl+s arrives as. `console` names the control bytes it recognises (ctrl+a is
/// `Key::Home`) and passes the rest through as the character they are; 0x13 is one of the rest.
/// It reaches the program at all only because raw mode (`cfmakeraw`) clears `IXON` — in a cooked
/// terminal the same byte is XOFF and freezes output instead.
const CTRL_S: char = '\u{13}';

/// The colour of an entry that would ANSWER something currently demanded — see [`Form::unmet`].
///
/// Green for the same reason suggestions are blue: it is the form pointing, not a state the user
/// set. Blue says "we think you want this"; green says "you must pick one of these". Both stop
/// as soon as the user has acted, and neither counts as a deviation from what the form opened
/// with, because neither is a claim about the machine.
const WANTED_GREEN: u8 = 42;

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
    // BUFFERED, and the difference is the whole of the flicker: see `_paint`. Same target and
    // same tty-ness as `Term::stderr()` — only the writes are pooled, until an explicit flush.
    let term = Term::buffered_stderr();
    if !term.is_term() {
        return Err(std::io::Error::other(
            "interactive forms need a terminal (stderr is not one)",
        ));
    }
    // Taken once, before a key is pressed: the whole point is to compare against what the CALLER
    // handed over, not against whatever the last repaint happened to see.
    let opened = Opened::of(form);
    let mut focus = 0;
    let mut on_screen = 0;
    let mut scroll = 0;
    let (fd, _tty_handle) = _input_fd()?;
    // Raw for the whole run (drops — and restores — on every path out of this function).
    let _raw = RawMode::engage(fd)?;
    term.hide_cursor()?;
    term.flush()?;
    let outcome = loop {
        // Recomputed per repaint rather than once, because folding a section adds and removes
        // rows: a list taken before the first keypress would go stale the moment one shut.
        let rows = focusables(form);
        // …and a shorter list must not be indexed at the old cursor. `_refocus_fold` already
        // moves the cursor somewhere sensible; this is the belt to its braces.
        focus = focus.min(rows.len().saturating_sub(1));
        let (rows_tall, width) = term.size();
        let mut drained = 0;
        // Asked again every repaint, so the cautions — and any emphasis the caller paints on
        // the rows — track the answers as they change.
        let notes = _notes(form, &mut warn);
        let frame = compose(form, &rows, focus, (width as usize).max(20), &notes, &opened);
        // One line fewer than the terminal has: a frame that fills it exactly ends with a newline
        // on the last row, which scrolls the screen, and the next repaint's climb back up would
        // land one row low and draw over the wrong lines from then on. Asked every repaint, so a
        // resized terminal is honoured on the next key.
        let height = (rows_tall as usize).saturating_sub(1).max(MIN_BODY + 2);
        let (shown, at) = viewport(&frame, height, scroll);
        scroll = at;
        // Repaint only when something changed — the inner loop below eats the junk events a
        // terminal produces (focus reports, stray escapes) without a single write.
        _paint(&term, &shown, on_screen)?;
        on_screen = shown.len();
        let action = loop {
            match _await_input(fd) {
                Ok(true) => {}
                // Hangup, or an unreadable terminal: nobody is there to answer.
                Ok(false) | Err(_) => break Action::Cancel,
            }
            match term.read_key() {
                // The terminal went away mid-question (hangup, ctrl-d): treat as walking off.
                Err(_) => break Action::Cancel,
                Ok(key) => {
                    // Taken fresh per KEY, not per frame: while several keys are being folded
                    // into one repaint, any of them may fold a section and change what rows
                    // exist. The list the next key is applied against has to know.
                    let live = focusables(form);
                    focus = focus.min(live.len().saturating_sub(1));
                    let action = apply(form, &live, &mut focus, key);
                    drained += 1;
                    // A key that changed nothing never painted anything anyway; a key that did,
                    // with more input already queued behind it, paints once for the whole burst.
                    // This is what stops a held arrow key from queueing a full render each.
                    if matches!(action, Action::Ignored)
                        || _coalesce(&action, _input_pending(fd), drained)
                    {
                        continue;
                    }
                    break action;
                }
            }
        };
        match action {
            Action::Submit => break Outcome::Submitted,
            Action::Cancel => break Outcome::Cancelled,
            _ => {}
        }
    };
    // The form comes off the screen on the way out, as it always did — but through the same one
    // write, so the last thing a user sees is not a block erasing itself a line at a time.
    //
    // A frame of NO lines against a block of `on_screen` is exactly what `clear_last_lines` did:
    // every row wiped, cursor left where the form began. Calling both would erase that many
    // lines again, above the form, taking whatever the caller had printed before it.
    _paint(&term, &[], on_screen)?;
    term.show_cursor()?;
    term.flush()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Choice, Condition, GridCell, GridRow, Rule};

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

        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
        options[1].checked = true;
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
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[1].enabled = false;
        options[1].checked = true; // it arrives ticked, and must stay ticked

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
        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!() };
        assert!(options[1].checked, "a fixed box keeps its state through every keystroke");

        // Ctrl+A: flips what is the user's, leaves what is not.
        let mut form = Form::new().checkboxes("Tops", &["a", "b", "c"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[1].enabled = false;
        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!() };
        let ticks = |options: &[Choice]| options.iter().map(|o| o.checked).collect::<Vec<_>>();
        assert_eq!(ticks(options), [true, false, true], "all-on skips the fixed box");
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!() };
        assert_eq!(
            ticks(options),
            [false, false, false],
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
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!() };
        options[0].enabled = false;

        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Char(' '));

        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!() };
        assert!(options[0].checked, "the one that was ticked");
        let Item::Checkboxes { options, .. } = &form.items[1] else { panic!() };
        assert!(!options[0].checked, "its locked twin stayed put");
    }

    /// Sub-titles draw dim above the option they belong to, and a disabled option draws dim
    /// itself — the two ways this form says "read, do not touch".
    #[test]
    fn sub_titles_and_locked_options_are_drawn_dim() {
        let mut form = Form::new().checkboxes("Packages", &["zed", "helix", "apt"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].heading = Some("dev-tools".into());
        options[2].heading = Some("system\n(not yours)".into());
        options[2].enabled = false;

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
                "↑/↓/←/→ move · space picks · ctrl+a all/none · ctrl+s submit · enter next/submit · esc cancels",
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
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;

        let opened = Opened::of(&form); // taken once, as a run does
        let rows = focusables(&form);
        let row = |form: &Form, want: &str| {
            render(form, &rows, rows.len() - 1, 60, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };

        // Focus parks on Submit throughout, so nothing here is the focus highlight's doing.
        assert_eq!(row(&form, "on"), "  [■] on", "unchanged, unmarked");
        assert_eq!(row(&form, "off"), "  [ ] off");

        // Clear the one that arrived ticked.
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = false;
        assert_eq!(row(&form, "on"), console::style("  [ ] on").red().to_string());

        // Tick the one that arrived clear: a plain answer, not a reversal.
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[1].checked = true;
        assert_eq!(row(&form, "off"), "  [x] off", "filling something in is not undoing it");

        // Put it back: the mark comes off as cleanly as it went on.
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
        assert_eq!(row(&form, "on"), "  [■] on");
    }

    /// Three managers, three packages, one unavailable everywhere but the middle — the shape the
    /// whole feature exists for. Boxes first so the columns read straight down, name after.
    // ——— folding sections ————————————————————————————————————————————————
    //
    // A form with sub-titles and nothing else, so the tests below can each turn ONE thing on.
    // `dev` covers slots 0–1, `web` covers 2–3, and slot 0 also opens the first section, which
    // is what makes "before any heading" a case worth having: there is no such slot here, and
    // `loose_options_never_fold` builds one that does.
    fn sectioned() -> Form {
        let mut form = Form::new().checkboxes("packages", &["git", "jq", "brave", "firefox"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].heading = Some("dev".into());
        options[2].heading = Some("web".into());
        options[1].checked = true; // something ticked inside a section, for the answers test
        form
    }

    fn drawn(form: &Form) -> Vec<String> {
        let rows = focusables(form);
        render(form, &rows, rows.len() - 1, 200, &[], &Opened::of(form))
            .iter()
            .map(|line| console::strip_ansi_codes(line).trim_end().to_string())
            .collect()
    }

    /// The opt-in, which is the whole contract: a form that does not ask for folding is drawn and
    /// walked exactly as it was before the feature existed.
    #[test]
    fn sub_titles_do_not_fold_unless_the_form_asks() {
        let plain = sectioned();
        let folding = sectioned().collapsible();

        let flat = drawn(&plain);
        assert!(flat.iter().any(|line| line.trim() == "dev"), "the sub-title still shows: {flat:#?}");
        assert!(
            !flat.iter().any(|line| line.contains('>') || line.trim_start().starts_with('v')),
            "and carries no marker: {flat:#?}"
        );
        assert_eq!(focusables(&plain).len(), 5, "four options and Submit — no section rows");
        assert_eq!(focusables(&folding).len(), 7, "…plus a `v` for each of two sections; no `^` is a stop");
        assert_eq!(
            focusables(&folding.clone().folded(0, 0)).len(),
            5,
            "shut, `dev` keeps its title as `>` and loses its two options"
        );
        // Nothing but the flag differs, so any render change is the flag's doing.
        assert_eq!(plain.items, folding.items, "folding never rewrites the items");
    }

    /// What an open section looks like: `v` above its options, `^` after the last of them.
    #[test]
    fn an_open_section_is_marked_at_both_ends() {
        let lines = drawn(&sectioned().collapsible());
        let at = |want: &str| lines.iter().position(|line| line.trim() == want);
        assert_eq!(
            (at("v dev"), at("[ ] git"), at("[■] jq"), at("^ dev")),
            (Some(1), Some(2), Some(3), Some(4)),
            "{lines:#?}"
        );
        assert_eq!((at("v web"), at("^ web")), (Some(5), Some(8)), "{lines:#?}");
    }

    /// And a shut one: `>`, nothing beneath it, and no `^` — there is nothing to close.
    #[test]
    fn a_shut_section_hides_its_options_and_drops_its_foot() {
        let form = sectioned().collapsible().folded(0, 0);
        let lines = drawn(&form);
        assert!(lines.iter().any(|line| line.trim() == "> dev"), "{lines:#?}");
        assert!(!lines.iter().any(|line| line.contains("git")), "folded away: {lines:#?}");
        assert!(!lines.iter().any(|line| line.contains("jq")), "folded away: {lines:#?}");
        assert!(!lines.iter().any(|line| line.trim() == "^ dev"), "nothing to close: {lines:#?}");
        // The other section is untouched — folding is per-section, not per-form.
        assert!(lines.iter().any(|line| line.trim() == "v web"), "{lines:#?}");
        assert!(lines.iter().any(|line| line.contains("brave")), "{lines:#?}");
    }

    /// The guarantee that makes folding safe: it hides a question, it does not withdraw it.
    #[test]
    fn folding_hides_options_without_touching_the_answers() {
        let open = sectioned().collapsible();
        let shut = sectioned().collapsible().folded(0, 0);
        assert_eq!(open.checked("packages"), ["jq"]);
        assert_eq!(shut.checked("packages"), ["jq"], "still ticked, merely out of sight");
        assert_eq!(open.answers_toml(), shut.answers_toml(), "and identical on the way out");
    }

    /// A folded option is unreachable by the same rule as a comment or a disabled box: the focus
    /// list has no row for it, so no key can name one.
    #[test]
    fn the_cursor_cannot_reach_inside_a_folded_section() {
        let form = sectioned().collapsible().folded(0, 0);
        let rows = focusables(&form);
        assert!(
            !rows.iter().any(|row| matches!(row, Focus::Option { option: 0 | 1, .. })),
            "{rows:#?}"
        );
        assert!(rows.contains(&Focus::Section { item: 0, slot: 0, foot: false }), "{rows:#?}");
        assert!(
            !rows.contains(&Focus::Section { item: 0, slot: 0, foot: true }),
            "a shut section has no foot"
        );
        // Walking the whole form with ↓ must never land on a hidden option, and must terminate.
        let mut focus = 0;
        let mut form = form;
        for _ in 0..rows.len() * 2 {
            let rows = focusables(&form);
            assert!(!matches!(rows[focus], Focus::Option { option: 0 | 1, .. }));
            apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        }
    }

    /// Space or → on a shut section's `>` opens it, and the cursor lands on the first entry inside
    /// rather than staying on the title. ← on a shut title has nothing left to do.
    #[test]
    fn opening_a_shut_section_lands_the_cursor_on_its_first_entry() {
        let mut form = sectioned().collapsible().folded(0, 0);
        let mut focus = 0; // the `> dev` row is first
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 0, foot: false });
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::ArrowLeft), Action::Ignored);
        assert!(form.collapsed.contains(&(0, 0)), "already shut — nothing to repaint");

        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char(' ')), Action::Redraw);
        assert!(!form.collapsed.contains(&(0, 0)), "space opened it");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Option { item: 0, option: 0 }, "and the cursor is inside");

        // Fold it again from inside, and → reopens it from the `>` the same way.
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Tab), Action::Redraw);
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 0, foot: false }, "back on the `>`");
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::ArrowRight), Action::Redraw);
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Option { item: 0, option: 0 }, "right opens, and lands inside");
    }

    /// A section's title is a stop, open or shut; its `^` closing line is drawn and skipped. So ↓
    /// from the last entry of one open section lands on the next section's title, and ↓ again on
    /// its first entry — the closing line is never in between.
    #[test]
    fn titles_are_stops_and_closing_markers_are_drawn_but_never_stops() {
        let mut form = sectioned().collapsible();
        let rows = focusables(&form);
        assert!(
            !rows.iter().any(|row| matches!(row, Focus::Section { foot: true, .. })),
            "no closing line is a stop: {rows:#?}"
        );
        assert_eq!(
            rows.iter().filter(|row| matches!(row, Focus::Section { foot: false, .. })).count(),
            2,
            "both open titles are"
        );
        let shown: Vec<String> = render(&form, &rows, 0, 80, &[], &Opened::of(&form))
            .iter()
            .map(|line| console::strip_ansi_codes(line).trim().to_string())
            .collect();
        assert!(shown.iter().any(|line| line.starts_with("v ")), "the title is drawn: {shown:#?}");
        assert!(shown.iter().any(|line| line.starts_with("^ ")), "and so is the closing line");

        let mut focus = rows.iter().position(|row| *row == Focus::Option { item: 0, option: 1 }).unwrap();
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 2, foot: false }, "over the `^`, onto `v web`");
        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(rows[focus], Focus::Option { item: 0, option: 2 }, "then into it");
    }

    /// The case that put the title back: a section with nothing selectable inside. Opening it has
    /// nowhere inside to land, so the cursor takes the title — which is the row that can fold it
    /// again. Without that, a block could be opened once and never shut.
    #[test]
    fn opening_a_section_with_nothing_selectable_inside_lands_on_its_title() {
        let mut form = Form::new()
            .collapsible()
            .grid(
                "",
                &["apt"],
                vec![
                    GridRow::named("locked").heading("out of reach").cells(vec![GridCell::locked(false)]),
                    GridRow::named("live").heading("live").cells(vec![GridCell::open(None)]),
                ],
            )
            .folded(0, 0);
        let rows = focusables(&form);
        let mut focus = 0; // the `> out of reach` row
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 0, foot: false });
        apply(&mut form, &rows, &mut focus, Key::ArrowRight);
        assert!(!form.collapsed.contains(&(0, 0)), "opened");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 0, foot: false }, "nothing inside: the title");
        apply(&mut form, &rows, &mut focus, Key::Tab);
        assert!(form.collapsed.contains(&(0, 0)), "and it folds again from there");
    }

    /// Options standing before the first sub-title belong to no section, and nothing can fold
    /// them away — otherwise a form could hide rows with no marker left to bring them back.
    #[test]
    fn loose_options_never_fold() {
        let mut form = Form::new().checkboxes("packages", &["loose", "git"]).collapsible();
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[1].heading = Some("dev".into());
        let form = form.folded(0, 1);

        let lines = drawn(&form);
        assert!(lines.iter().any(|line| line.contains("loose")), "outside any section: {lines:#?}");
        assert!(!lines.iter().any(|line| line.contains("git")), "inside a shut one: {lines:#?}");
        assert!(!form.hidden(0, 0), "no heading governs slot 0");
        assert!(form.hidden(0, 1));
    }

    /// Grids fold on their row headings, the same way and by the same code.
    #[test]
    fn a_grid_folds_on_its_row_headings() {
        let form = grid_form().collapsible();
        let lines = drawn(&form);
        assert!(lines.iter().any(|line| line.trim() == "v # tools"), "{lines:#?}");
        assert!(lines.iter().any(|line| line.trim() == "^ # tools"), "{lines:#?}");

        let form = grid_form().collapsible().folded(0, 1); // "# browsers", covering rows 1 and 2
        let lines = drawn(&form);
        assert!(lines.iter().any(|line| line.contains("git")), "other section still open");
        assert!(!lines.iter().any(|line| line.contains("brave")), "{lines:#?}");
        assert!(!lines.iter().any(|line| line.contains("firefox")), "a section is not one row");
        let rows = focusables(&form);
        assert!(!rows.iter().any(|row| matches!(row, Focus::Cell { row: 1 | 2, .. })), "{rows:#?}");
        assert_eq!(form.answers_toml(), grid_form().answers_toml(), "answers unchanged");
    }

    /// A multi-line sub-title gets one marker, on its first line. Four arrows down the left of
    /// one title would read as four sections.
    #[test]
    fn only_the_first_line_of_a_heading_is_marked() {
        let mut form = Form::new().checkboxes("p", &["a"]).collapsible();
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].heading = Some("first\nsecond".into());

        let lines = drawn(&form);
        assert_eq!(
            lines.iter().map(|line| line.trim()).take(5).collect::<Vec<_>>(),
            ["p:", "v first", "second", "[ ] a", "^ first"],
            "the head carries both lines and one arrow; the foot names the section and stops"
        );
        let marked = |arrow: char| {
            lines.iter().filter(|line| line.trim_start().starts_with(arrow)).count()
        };
        assert_eq!((marked('v'), marked('^')), (1, 1), "one section, one of each: {lines:#?}");
    }

    /// The whole shape, in one place — a folded section and an open one, drawn together. The
    /// other tests check pieces; this one is what a reader actually sees, and is the test that
    /// fails if the arrangement drifts.
    #[test]
    fn a_folding_grid_draws_as_the_shape_it_was_asked_for() {
        let cell = || GridCell::open(None);
        let row = |label: &str, heading: Option<&str>| {
            let row = GridRow::named(label).cells(vec![cell(), cell()]);
            heading.map_or_else(|| row.clone(), |said| row.clone().heading(said))
        };
        let form = Form::new()
            .collapsible()
            .grid(
                "",
                &["apt", "flatpak"],
                vec![
                    row("firefox", Some("Browsers")),
                    row("brave", None),
                    row("nautilus", Some("File managers")),
                ],
            )
            .folded(0, 0);

        assert_eq!(
            drawn(&form),
            [
                // Leading spaces kept, not trimmed: the columns ARE the assertion. Every row of
                // a folding grid — header, fold markers, boxes — starts at the same place.
                "  > Browsers",
                "  v File managers",
                "  apt  flatpak",
                "  [ ]  [ ]      nautilus",
                "  ^ File managers",
                "",
                "▸ [ Submit ]",
                "↑/↓/←/→ move · space picks · ctrl+a all/none · ctrl+s submit · tab fold/unfold · enter next/submit · esc cancels",
            ]
        );
        // …and without the flag, the table sits flush as it always did.
        let plain = drawn(&Form::new().grid("", &["apt", "flatpak"], vec![row("firefox", None)]));
        assert_eq!(plain[0], "apt  flatpak", "no marker column when nothing folds");
    }

    /// A row with more cells than the grid has columns. Nothing forbids it, `answers_toml` has
    /// always dropped the excess, and `render` used to index off the end of the column widths and
    /// take the whole process down mid-paint.
    #[test]
    fn a_row_wider_than_its_grid_draws_instead_of_panicking() {
        let form = Form::new().grid(
            "packages",
            &["apt"],
            vec![GridRow::named("git").cells(vec![GridCell::set(None), GridCell::set(None), GridCell::set(None)])],
        );
        // ["packages:", "apt", the row, …] — the label line, then the headings, then the boxes.
        assert_eq!(drawn(&form)[2], "[■]  git", "one column drawn, the surplus dropped");
        assert!(form.answers_toml().contains("apt"), "and the answers agree: {}", form.answers_toml());
    }

    /// Why `run` recomputes the focus list per repaint, rather than taking it once as it used to.
    ///
    /// Not because folding loses anything — the answers are untouched, which the test above
    /// pins. Because the LIST is not data: it is the set of rows that exist to be landed on, and
    /// a shut section has two fewer of them (its options); its title stays, `>` now where `v` was.
    ///
    /// Held here because `run`'s loop needs a terminal, so the staleness itself cannot be tested
    /// where it lives. This models the loop instead: take a list once, fold, and keep using it.
    #[test]
    fn a_stale_focus_list_would_strand_the_cursor_on_an_invisible_row() {
        let mut form = sectioned().collapsible();
        let stale = focusables(&form); // what a list taken once, before any key, would hold
        let mut focus = 0;
        assert_eq!(stale[focus], Focus::Section { item: 0, slot: 0, foot: false }, "`v dev` is first");
        apply(&mut form, &stale, &mut focus, Key::Tab);
        assert!(form.collapsed.contains(&(0, 0)), "the first section is now shut");

        let fresh = focusables(&form);
        assert_eq!((stale.len(), fresh.len()), (7, 5), "two options stopped existing");

        // Step down against the list as it WAS. It lands on an option inside the shut section…
        let mut adrift = 0;
        apply(&mut form.clone(), &stale, &mut adrift, Key::ArrowDown);
        assert!(matches!(stale[adrift], Focus::Option { option: 0, .. }), "{:?}", stale[adrift]);
        // …and that row is not drawn, so the cursor is nowhere on screen. This is the bug.
        let gone = render(&form, &stale, adrift, 200, &[], &Opened::of(&form));
        assert!(
            !gone.iter().any(|line| line.contains('▸')),
            "no row carries the cursor: {gone:#?}"
        );

        // Step down against a fresh list, and it lands on the next thing actually visible.
        let mut live = 0;
        apply(&mut form.clone(), &fresh, &mut live, Key::ArrowDown);
        assert_eq!(fresh[live], Focus::Section { item: 0, slot: 2, foot: false });
        let shown = render(&form, &fresh, live, 200, &[], &Opened::of(&form));
        assert_eq!(
            shown.iter().filter(|line| line.contains('▸')).count(),
            1,
            "exactly one, and it is on screen: {shown:#?}"
        );
    }

    // ——— suggestions ——————————————————————————————————————————————————————

    /// Three ticks, three meanings: one the machine already has, one the form recommends, one
    /// empty. The middle one is the case under test.
    fn suggested_form() -> Form {
        let mut form = Form::new().checkboxes("packages", &["installed", "recommended", "neither"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
        options[1].checked = true;
        options[1].suggested = true;
        form
    }

    /// The whole of what a suggestion is: it arrives ticked, and clearing it is not a deviation.
    /// A tick the form ASSERTED still reddens when cleared — the two directions are the point.
    #[test]
    fn declining_a_suggestion_is_not_marked_but_undoing_a_fact_still_is() {
        let mut form = suggested_form();
        let opened = Opened::of(&form); // taken once, as a run does
        let rows = focusables(&form);
        let row = |form: &Form, want: &str| {
            render(form, &rows, rows.len() - 1, 80, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };

        // Clear both ticked boxes.
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = false;
        options[1].checked = false;

        assert_eq!(
            row(&form, "installed"),
            console::style("  [ ] installed").red().to_string(),
            "a fact undone is worth seeing"
        );
        assert_eq!(
            row(&form, "recommended"),
            "  [ ] recommended",
            "declining a recommendation is an ordinary answer, and unmarked"
        );
    }

    /// While it stands, a suggestion is blue — distinct from the plain tick of something already
    /// installed, which is the distinction the colour exists to draw.
    #[test]
    fn a_standing_suggestion_is_blue_and_a_plain_tick_is_not() {
        let form = suggested_form();
        let rows = focusables(&form);
        let lines = render(&form, &rows, rows.len() - 1, 80, &[], &Opened::of(&form));
        let row = |want: &str| {
            lines.iter().find(|line| console::strip_ansi_codes(line).contains(want)).expect("drawn")
        };

        assert_eq!(row("installed"), "  [■] installed", "already so — no colour");
        assert_eq!(
            row("recommended"),
            &format!("  {}", console::style("[x] recommended").color256(SUGGESTED_BLUE)),
            "recommended — blue"
        );
        assert_eq!(row("neither"), "  [ ] neither");
    }

    /// A suggestion is an answer like any other. Blue is about where the tick CAME FROM, not
    /// about whether it counts.
    #[test]
    fn a_suggestion_counts_in_the_answers() {
        let form = suggested_form();
        assert_eq!(form.checked("packages"), ["installed", "recommended"]);
        assert!(form.answers_toml().contains("recommended"), "{}", form.answers_toml());
    }

    /// Grids say it with `GridCell::suggest`, and behave identically — same exemption, same blue.
    #[test]
    fn a_grid_cell_can_be_suggested_too() {
        let row = |label: &str, cell: GridCell| GridRow::named(label).cells(vec![cell]);
        let mut form = Form::new().grid(
            "packages",
            &["apt"],
            vec![row("installed", GridCell::set(None)), row("recommended", GridCell::open(None).suggest())],
        );
        let opened = Opened::of(&form);
        let rows = focusables(&form);
        // By content rather than by index: a grid draws a label line and a header above its rows,
        // and counting past them is how the first version of this test went wrong.
        let row_of = |form: &Form, want: &str| {
            render(form, &rows, rows.len() - 1, 80, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };
        let boxed = |mark: &str| format!("{}  {mark}", console::style("[x]").color256(SUGGESTED_BLUE));

        assert_eq!(row_of(&form, "recommended"), boxed("recommended"), "suggested — blue");
        assert_eq!(row_of(&form, "installed"), "[■]  installed", "already so — plain");

        // Clear both. Only the one the form ASSERTED reddens.
        let Item::Grid { rows: grid, .. } = &mut form.items[0] else { panic!() };
        grid[0].cells[0].checked = false;
        grid[1].cells[0].checked = false;
        assert_eq!(
            row_of(&form, "installed"),
            format!("{}  installed", console::style("[ ]").red()),
            "a fact undone"
        );
        assert_eq!(row_of(&form, "recommended"), "[ ]  recommended", "a suggestion declined");
    }

    /// Re-ticking a suggestion restores the blue: the box is standing again, and where it came
    /// from has not changed. It never becomes red, in either direction.
    #[test]
    fn a_suggestion_toggled_off_and_on_is_blue_again_and_never_red() {
        let mut form = suggested_form();
        let opened = Opened::of(&form);
        let rows = focusables(&form);
        let mut focus = rows.iter().position(|r| *r == Focus::Option { item: 0, option: 1 }).unwrap();
        let line = |form: &Form| {
            render(form, &rows, rows.len() - 1, 80, &[], &opened)
                .into_iter()
                .find(|l| console::strip_ansi_codes(l).contains("recommended"))
                .expect("drawn")
        };

        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(line(&form), "  [ ] recommended", "off, and not red");
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(
            line(&form),
            format!("  {}", console::style("[x] recommended").color256(SUGGESTED_BLUE)),
            "on again, and blue again"
        );
    }

    // ——— filtering by tag ————————————————————————————————————————————————

    /// Three rows across two sections, tagged so that every case has an example: one plain,
    /// one carrying two tags, one carrying none at all.
    fn tagged() -> Form {
        let row = |label: &str, heading: Option<&str>, tags: &[&str]| {
            let row = GridRow::named(label).cells(vec![GridCell::open(None)]).tags(tags);
            heading.map_or_else(|| row.clone(), |said| row.clone().heading(said))
        };
        Form::new()
            .grid(
                "packages",
                &["apt"],
                vec![
                    row("ripgrep", Some("# tools"), &["terminal"]),
                    // Its own section, so filtering it away empties one. Without a heading it
                    // would fall into `# apps` below and keep that section alive — which is
                    // correct, and caught this fixture out when it was written the other way.
                    row("zoom", Some("# apps"), &["gui", "spyware"]),
                    row("mystery", Some("# unknown"), &[]),
                ],
            )
            .filters(
                "Include",
                &[
                    // The negative half is the point: a row carrying BOTH is neither.
                    (&["terminal", "!gui"][..], "terminal-only"),
                    (&["gui", "!terminal"][..], "GUI-only"),
                    (&["spyware"][..], "spyware"),
                ],
            )
    }

    /// The block itself: one row per offered tag, all ticked, above everything it governs — and
    /// it is not an item, so it never reaches the answers.
    #[test]
    fn the_filter_block_draws_above_the_form_and_is_not_an_answer() {
        let form = tagged();
        let lines = drawn(&form);
        assert_eq!(
            lines.iter().map(|l| l.trim()).take(5).collect::<Vec<_>>(),
            // Each box counts what it governs, in a column: labels padded, counts right-aligned.
            ["Include:", "[x] terminal-only (1)", "[x] GUI-only      (1)", "[x] spyware       (1)", ""],
            "{lines:#?}"
        );
        assert!(!form.answers_toml().contains("Include"), "{}", form.answers_toml());
        assert!(!form.answers_toml().contains("terminal-only"), "a filter is not a question");
    }

    /// The filter block is violet, and nothing else is — it is the one block on screen that is
    /// not part of the answers, so it does not look like the things that are. Focus still
    /// reverses it, which means the colour has to survive being re-armed through the reverse
    /// video rather than ending it halfway along the row.
    #[test]
    fn the_filter_block_is_coloured_and_stays_coloured_under_the_cursor() {
        // Expectations built through `console::style` like every other colour test here, because
        // `style` emits nothing when stdout is not a terminal — which under `cargo test` it never
        // is. Comparing styled against styled holds either way, and checks the real thing when a
        // run does have colour.
        let violet = |text: &str| console::style(text).color256(FILTER_VIOLET).to_string();
        let form = tagged();
        let rows = focusables(&form);
        let lines = render(&form, &rows, rows.len() - 1, 200, &[], &Opened::of(&form));

        assert_eq!(lines[0], violet("Include:"), "the heading");
        assert_eq!(lines[1], format!("  {}", violet("[x] terminal-only (1)")), "and each box");
        // The rows the filter GOVERNS keep the plain form every other entry has. Asserted as
        // equality rather than "is not violet": with colour off, `violet("")` is the empty
        // string and `contains` of it is true of everything — a check that could never fail.
        let ripgrep = lines.iter().find(|l| l.contains("ripgrep")).expect("drawn");
        assert_eq!(ripgrep, "[ ]  ripgrep", "an entry is styled by nothing here");

        // Focused, the colour is re-armed after its own reset so the reverse video runs the whole
        // row instead of stopping where the violet ends.
        let at = rows.iter().position(|row| *row == Focus::Filter { at: 0 }).unwrap();
        let focused = &render(&form, &rows, at, 200, &[], &Opened::of(&form))[1];
        let styled = violet("[x] terminal-only (1)");
        assert_eq!(
            *focused,
            format!("\x1b[7m▸ {}\x1b[0m", styled.replace("\x1b[0m", "\x1b[0m\x1b[7m"))
        );
    }

    /// The semantics, and the whole reason the choice mattered: an entry is hidden when it
    /// carries ANY cleared tag. Clearing `spyware` removes the spyware even though it is also
    /// tagged `gui` and `gui` is still ticked. A filter that leaked here would be useless for
    /// the thing filters are mostly wanted for.
    #[test]
    fn any_cleared_tag_hides_an_entry_even_when_another_of_its_tags_stays() {
        let mut form = tagged();
        assert!(!form.filtered_out(0, 1), "everything shows to begin with");

        form.excluded.insert("spyware".into());
        assert!(form.filtered_out(0, 1), "zoom carries spyware, so zoom goes");
        assert!(!form.filtered_out(0, 0), "ripgrep carries neither");
        assert!(!form.filtered_out(0, 2), "and an untagged row can never be excluded");

        let lines = drawn(&form);
        assert!(!lines.iter().any(|line| line.contains("zoom")), "{lines:#?}");
        assert!(lines.iter().any(|line| line.contains("ripgrep")), "{lines:#?}");
        assert!(lines.iter().any(|line| line.contains("mystery")), "{lines:#?}");
    }

    /// The case a tag-per-box cannot express, and the reason rules exist.
    ///
    /// Something carrying BOTH `terminal` and `gui` is neither terminal-only nor GUI-only, so
    /// clearing either box must leave it alone. Under the old one-tag scheme it vanished from
    /// both — the wrong answer twice, and invisibly, since the row simply was not there to argue
    /// with.
    #[test]
    fn an_entry_carrying_both_tags_is_governed_by_neither_only_box() {
        let row = |label: &str, tags: &[&str]| GridRow::named(label).cells(vec![GridCell::open(None)]).tags(tags);
        let mut form = Form::new()
            .grid(
                "packages",
                &["apt"],
                vec![
                    row("ripgrep", &["terminal"]),
                    row("firefox", &["gui"]),
                    row("code", &["terminal", "gui"]), // ships both a CLI and a window
                ],
            )
            .filters(
                "Include",
                &[(&["terminal", "!gui"][..], "terminal-only"), (&["gui", "!terminal"][..], "GUI-only")],
            );

        form.excluded.insert("terminal-only".into());
        assert!(form.filtered_out(0, 0), "ripgrep is terminal and not gui");
        assert!(!form.filtered_out(0, 1), "firefox is untouched by the terminal box");
        assert!(!form.filtered_out(0, 2), "and so is the one that is both");

        // Clear BOTH boxes. The pure ones go; the one that is both survives, which is the whole
        // distinction — "only" is a claim about what a thing is NOT.
        form.excluded.insert("GUI-only".into());
        assert!(form.filtered_out(0, 0));
        assert!(form.filtered_out(0, 1));
        assert!(!form.filtered_out(0, 2), "neither box governs it, so nothing hides it");
        let lines = drawn(&form);
        assert!(lines.iter().any(|line| line.contains("code")), "{lines:#?}");
    }

    /// `!` is read once, at construction, and splits the terms into the two lists.
    #[test]
    fn a_rule_reads_its_negations_when_it_is_built() {
        let rule = Rule::of("terminal-only", &["terminal", "!gui", "!legacy"]);
        assert_eq!(rule.all_of, ["terminal"]);
        assert_eq!(rule.none_of, ["gui", "legacy"]);
        assert!(rule.matches(&["terminal".into()]));
        assert!(!rule.matches(&["terminal".into(), "legacy".into()]), "any forbidden tag is enough");
        assert!(!rule.matches(&[]), "a positive term must actually be there");
        // Several positives all have to hold.
        let both = Rule::of("both", &["terminal", "gui"]);
        assert!(both.matches(&["terminal".into(), "gui".into()]));
        assert!(!both.matches(&["terminal".into()]));
    }

    /// Hiding a row is not answering for it: a filtered entry keeps every tick it had, and comes
    /// back with them when the tag is restored.
    #[test]
    fn filtering_never_touches_the_answers() {
        let mut form = tagged();
        let Item::Grid { rows, .. } = &mut form.items[0] else { panic!() };
        rows[1].cells[0].checked = true; // zoom, ticked
        let before = form.answers_toml();
        assert!(before.contains("zoom"), "{before}");

        form.excluded.insert("spyware".into());
        assert!(form.filtered_out(0, 1), "out of sight");
        assert_eq!(form.answers_toml(), before, "…and still in the answers");

        form.excluded.remove("spyware");
        assert!(!form.filtered_out(0, 1), "and back again, unchanged");
        assert_eq!(form.answers_toml(), before);
    }

    /// A filtered row is unreachable, by the same rule as a folded or disabled one.
    #[test]
    fn the_cursor_skips_filtered_rows() {
        let mut form = tagged();
        form.excluded.insert("spyware".into());
        let rows = focusables(&form);
        assert!(!rows.iter().any(|row| matches!(row, Focus::Cell { row: 1, .. })), "{rows:#?}");
        assert!(rows.iter().any(|row| matches!(row, Focus::Cell { row: 0, .. })));
        assert_eq!(rows[0], Focus::Filter { at: 0 }, "the filter leads the list");
    }

    /// Space toggles a filter box, and the cursor stays on it even though the rows below have
    /// just appeared or vanished under it.
    #[test]
    fn space_toggles_a_filter_and_the_cursor_holds_its_place() {
        let mut form = tagged();
        let rows = focusables(&form);
        let mut focus = rows.iter().position(|r| *r == Focus::Filter { at: 2 }).unwrap();

        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char(' ')), Action::Redraw);
        assert!(form.excluded.contains("spyware"), "cleared");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Filter { at: 2 }, "still on the box just pressed");

        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert!(!form.excluded.contains("spyware"), "and back");
    }

    /// A section the filter empties disappears entirely — heading, markers and all. A `v` over
    /// nothing is a control that folds nothing.
    #[test]
    fn a_section_emptied_by_the_filter_is_not_drawn_at_all() {
        let mut form = tagged().collapsible();
        assert!(drawn(&form).iter().any(|line| line.trim() == "v # apps"));

        form.excluded.insert("spyware".into()); // "# apps" holds only zoom
        let lines = drawn(&form);
        assert!(!lines.iter().any(|line| line.contains("# apps")), "{lines:#?}");
        assert!(lines.iter().any(|line| line.contains("# tools")), "the other one stays");
        assert!(form.section_emptied(0, 1));
        assert!(!form.section_emptied(0, 0));
        let rows = focusables(&form);
        assert!(
            !rows.iter().any(|row| matches!(row, Focus::Section { slot: 1, .. })),
            "and its fold rows go with it: {rows:#?}"
        );
    }

    /// Filtering works whether or not the form folds — they are independent flags, and `hidden`
    /// is one predicate over both so nothing downstream has to check for each.
    #[test]
    fn filtering_and_folding_are_independent() {
        let mut plain = tagged();
        let mut folding = tagged().collapsible();
        // Keyed by the BOX's wording now, not by a tag — a rule is not a tag.
        plain.excluded.insert("terminal-only".into());
        folding.excluded.insert("terminal-only".into());
        assert!(!drawn(&plain).iter().any(|line| line.contains("ripgrep")), "filter alone");
        assert!(!drawn(&folding).iter().any(|line| line.contains("ripgrep")), "and with folding");

        // Folded AND filtered: still hidden, and unfolding does not bring back a filtered row.
        let form = tagged().collapsible().folded(0, 1);
        assert!(form.hidden(0, 1), "folded");
        let mut form = form;
        form.excluded.insert("spyware".into());
        form.collapsed.clear();
        assert!(form.hidden(0, 1), "unfolded, but still filtered out");
    }

    // ——— repainting ——————————————————————————————————————————————————————

    /// Net vertical movement of a frame: down for every newline, up for every `\x1b[nA`.
    fn drift(frame: &str) -> isize {
        let downs = frame.matches('\n').count() as isize;
        let ups: isize = frame
            .split("\x1b[")
            .filter_map(|part| part.split_once('A'))
            .filter(|(digits, _)| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            .filter_map(|(digits, _)| digits.parse::<isize>().ok())
            .sum();
        downs - ups
    }

    /// The whole repaint is ONE string, and it never blanks anything before drawing it. Those two
    /// together are what stopped the flicker: `Term::stderr()` is unbuffered, so the old
    /// clear-then-print cost `3n + 2` separate writes — 122 of them for a 40-line form — and a
    /// terminal renders as they arrive.
    #[test]
    fn a_repaint_is_one_write_that_overwrites_rather_than_clearing() {
        let frame = _frame(&["one".into(), "two".into()], 2);
        assert_eq!(frame, "\x1b[2Aone\x1b[K\ntwo\x1b[K\n");
        assert!(!frame.contains("\x1b[2K"), "nothing is blanked whole: {frame:?}");
        assert!(!frame.contains("\x1b[2J"), "and the screen is never cleared: {frame:?}");
    }

    /// The first paint has nothing above it to step back over.
    #[test]
    fn the_first_frame_does_not_move_the_cursor_up() {
        let frame = _frame(&["only".into()], 0);
        assert_eq!(frame, "only\x1b[K\n");
        assert_eq!(drift(&frame), 1, "one line drawn, cursor one below where it began");
    }

    /// A frame that shrank must wipe what it no longer covers — folding a section is exactly
    /// this, and without it the tail of the old form stays on screen under the new one.
    #[test]
    fn a_shorter_frame_erases_the_rows_it_gave_up() {
        let frame = _frame(&["kept".into()], 4);
        assert_eq!(frame, "\x1b[4Akept\x1b[K\n\x1b[K\n\x1b[K\n\x1b[K\n\x1b[3A");
        assert_eq!(drift(&frame), -3, "four rows became one: three higher than before");
    }

    /// Growing needs no wiping: the new rows land on ground the old frame never held.
    #[test]
    fn a_longer_frame_just_writes_the_extra_rows() {
        let frame = _frame(&["a".into(), "b".into(), "c".into()], 1);
        assert_eq!(frame, "\x1b[1Aa\x1b[K\nb\x1b[K\nc\x1b[K\n");
        assert_eq!(drift(&frame), 2);
    }

    /// The invariant every repaint depends on: afterwards the cursor sits immediately below the
    /// frame just drawn, so the next call finds the block by stepping up its own height. Checked
    /// across every shape, because getting it wrong by one drifts the form down the screen a row
    /// per keystroke — which is the other way a form flickers.
    #[test]
    fn the_cursor_always_lands_just_under_the_new_frame() {
        for previous in 0..6 {
            for height in 0..6 {
                let lines: Vec<String> = (0..height).map(|n| format!("line {n}")).collect();
                let frame = _frame(&lines, previous);
                assert_eq!(
                    drift(&frame),
                    height as isize - previous as isize,
                    "{height} lines over {previous}: {frame:?}"
                );
            }
        }
    }

    /// Teardown is the empty frame: everything erased, cursor back where the form began. It
    /// replaces `clear_last_lines`, and doing BOTH would erase that many rows again — above the
    /// form, taking whatever the caller had printed before it.
    #[test]
    fn an_empty_frame_removes_the_form_and_rewinds() {
        let frame = _frame(&[], 3);
        assert_eq!(frame, "\x1b[3A\x1b[K\n\x1b[K\n\x1b[K\n\x1b[3A");
        assert_eq!(drift(&frame), -3, "back to where the form started");
        assert!(!frame.contains("line"), "and nothing drawn");
    }

    /// Holding an arrow key used to queue one full repaint per keystroke, and a big grid takes
    /// long enough to draw that the backlog showed as the form freezing for seconds. Keys are
    /// now folded into one frame while more are already waiting.
    ///
    /// The rule that must never bend is the last two assertions: ending the run is never
    /// deferred for a tidier moment.
    #[test]
    fn keystrokes_are_folded_into_one_frame_but_never_the_last_one() {
        assert!(_coalesce(&Action::Redraw, true, 0), "more waiting — no need to paint yet");
        assert!(!_coalesce(&Action::Redraw, false, 0), "nothing waiting — paint now");
        assert!(
            !_coalesce(&Action::Redraw, true, COALESCE),
            "a flood cannot hold the screen still for ever"
        );
        assert!(!_coalesce(&Action::Submit, true, 0), "submitting ends the run, queue or no queue");
        assert!(!_coalesce(&Action::Cancel, true, 0), "and so does cancelling");
    }

    /// Ctrl+A means every box, including the ones currently folded away. The alternative — "all
    /// the boxes you can see" — would make the answer depend on which sections happen to be open,
    /// which is a worse surprise than a hidden box changing.
    #[test]
    fn tick_all_reaches_into_folded_sections() {
        let mut form = sectioned().collapsible().folded(0, 0);
        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Home);
        assert_eq!(form.checked("packages"), ["git", "jq", "brave", "firefox"], "all four");
    }

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
                GridRow::named("git")
                    .heading("# tools")
                    .note("version control")
                    .cells(vec![cell(true, true), GridCell::blank(), GridCell::blank()]),
                GridRow::named("brave")
                    .heading("# browsers")
                    .cells(vec![GridCell::blank(), cell(true, true), cell(false, true)]),
                GridRow::named("firefox").cells(vec![cell(false, true), cell(false, true), cell(false, true)]),
            ],
        )
    }

    /// The layout: a heading row of column names, a sub-title where one was given, then one line
    /// per row — boxes, then the label. (A FOLDING grid repeats the names under each open title
    /// instead; see `a_folding_grid_heads_each_open_block_instead_of_the_whole`.) Unavailable cells are dim and not boxes at
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
                "[■]   ·        ·    git      # version control",
                "# browsers",
                " ·   [■]      [ ]   brave",
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

    /// Neither a gap nor a locked box has a focus row, so no key can reach either — the same
    /// guarantee a disabled option has, by the same mechanism. They are drawn differently (a box
    /// says the choice exists, a gap says it does not) and reached identically: not at all.
    #[test]
    fn a_locked_box_and_a_gap_are_both_unreachable() {
        let mut form = Form::new().grid(
            "",
            &["apt", "snap", "flatpak"],
            vec![GridRow::named("neovim").cells(vec![
                    GridCell::open(Some("install".into())),
                    GridCell::locked(false),
                    GridCell::blank(),
                ])],
        );
        let rows = focusables(&form);
        assert_eq!(
            rows,
            [Focus::Cell { item: 0, row: 0, column: 0 }, Focus::Submit],
            "only the live cell, so sideways has nowhere to wander: {rows:?}"
        );

        let mut focus = 0;
        for _ in 0..12 {
            apply(&mut form, &rows, &mut focus, Key::Char(' '));
            apply(&mut form, &rows, &mut focus, Key::ArrowRight);
        }
        let Item::Grid { rows: grid, .. } = &form.items[0] else { panic!() };
        assert!(!grid[0].cells[1].checked, "a locked box stayed put through every keystroke");
        assert!(!grid[0].cells[2].checked, "and so did the gap");
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
            "[■]   ·        ·    git      # version control",
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
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!() };
        options[0].checked = true;
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
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!() };
        options[1].checked = true;
        let Item::Radio { chosen, .. } = &mut form.items[2] else { panic!() };
        *chosen = Some(0);
        let rows = focusables(&form);
        let lines = render(&form, &rows, 0, 120, &[], &Opened::of(&form));
        let plain: Vec<String> =
            lines.iter().map(|l| console::strip_ansi_codes(l).into_owned()).collect();
        let all = plain.join("\n");
        // `b` was ticked before the snapshot was taken, so it is a fact and draws as the given glyph.
        assert!(all.contains("[■] b") && all.contains("[ ] a"), "{all}");
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

    // ——— greying, green, and a Submit that refuses ————————————————————————

    /// Two rows, one of them something this machine cannot run.
    fn sessions(picked: bool) -> Form {
        let row = |label: &str, tag: &str, on: bool| {
            GridRow::named(label)
                .note(format!("a {tag} thing"))
                .cells(vec![if on { GridCell::set(None) } else { GridCell::open(None) }])
                .tags(&[tag])
        };
        Form::new()
            .grid("", &["apt"], vec![row("hyprland", "wayland-only", picked), row("i3", "x11-only", false)])
            .incompatible(&[(&["wayland-only"], "this session is X11")])
    }

    /// Greyed is not hidden, and that is the whole point: the row still draws, so a reader can
    /// see the choice exists and that something about the machine puts it out of reach. What it
    /// loses is the cursor — there is no focus row for it, which IS the "not yours to change".
    #[test]
    fn an_incompatible_row_still_draws_but_no_key_can_reach_it() {
        let form = sessions(false);
        let rows = focusables(&form);
        assert!(
            !rows.contains(&Focus::Cell { item: 0, row: 0, column: 0 }),
            "the wayland row has no cell to land on: {rows:?}"
        );
        assert!(rows.contains(&Focus::Cell { item: 0, row: 1, column: 0 }), "the X11 one does");

        let drawn = render(&form, &rows, 0, 100, &[], &Opened::of(&form));
        let flat = drawn.join("\n");
        assert!(flat.contains("hyprland"), "still on screen, greyed rather than gone");
        assert!(flat.contains("i3"));
    }

    /// An incompatibility earns a filter box, and it is a different offer from every other box
    /// in the block: not "do you want these" but "shall I stop showing you these". Safe to offer
    /// precisely because it can only ever hide rows that were already out of reach.
    ///
    /// It also explains the greying. A dim row with no reason is a puzzle; a dim row plus a box
    /// naming what is wrong with it is a sentence, which is why the rule carries one label and
    /// not two.
    #[test]
    fn an_incompatibility_gets_a_box_that_hides_only_what_was_already_locked() {
        let mut form = sessions(false);
        assert_eq!(
            form.filter_boxes().map(|rule| rule.label.as_str()).collect::<Vec<_>>(),
            ["this session is X11"],
            "no filters were offered, so the block is the incompatibility alone"
        );

        let shown = |form: &Form| {
            let rows = focusables(form);
            let flat = render(form, &rows, 0, 100, &[], &Opened::of(form)).join("\n");
            (flat.contains("hyprland"), flat.contains("i3"))
        };
        assert_eq!(shown(&form), (true, true), "both draw, one of them greyed");

        // Clear the box, the way a user would: find its row and press space.
        let rows = focusables(&form);
        let at = rows.iter().position(|row| matches!(row, Focus::Filter { .. })).expect("a box");
        let mut focus = at;
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert!(form.excluded.contains("this session is X11"));
        assert_eq!(shown(&form), (false, true), "the unrunnable row is gone; the other is not");

        // And back — nothing was withdrawn, only put away.
        let rows = focusables(&form);
        apply(&mut form, &rows, &mut focus, Key::Char(' '));
        assert_eq!(shown(&form), (true, true));
    }

    /// The requirement machinery through a CHECKBOX group — everything else exercises grids, and
    /// an untested second path is where the two quietly diverge. Same contract end to end: the
    /// demand appears when the dependent is ticked, the supplier goes green, Submit refuses, and
    /// picking the supplier settles all three.
    #[test]
    fn a_checkbox_requirement_points_blocks_and_settles_like_a_grid_one() {
        let mut form = Form::new().checkboxes("players", &["ncmpcpp", "mpd", "mpv"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!("boxes") };
        options[0].requires = vec!["audio-backend".into()];
        options[1].tags = vec!["audio-backend".into()];

        assert!(form.objections().is_empty(), "nothing ticked, nothing owed");
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!("boxes") };
        options[0].checked = true;
        assert_eq!(form.unmet(), ["audio-backend"]);
        assert!(form.wanted_at(0, 1) && !form.wanted_at(0, 2), "mpd would answer; mpv would not");

        // The pointing is drawn: the supplier's row takes the green, its neighbours do not.
        let rows = focusables(&form);
        let drawn = render(&form, &rows, 0, 100, &[], &Opened::of(&form));
        let row = |want: &str| {
            drawn
                .iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
                .clone()
        };
        assert_eq!(
            row("mpd"),
            format!("  {}", console::style("[ ] mpd").color256(WANTED_GREEN)),
            "the supplier, green"
        );
        assert_eq!(row("mpv"), "  [ ] mpv", "a bystander, plain");

        // Refused at the button, settled by the pick, exactly as the grid path is.
        let submit = rows.iter().position(|row| *row == Focus::Submit).expect("a submit row");
        let mut focus = submit;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Enter), Action::Ignored);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!("boxes") };
        options[1].checked = true;
        assert!(form.objections().is_empty());
        assert!(!form.wanted_at(0, 1), "picked, so the form stops pointing");
        let mut focus = submit;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Enter), Action::Submit);
    }

    /// Select-all reaches exactly the boxes the cursor could: a rule-greyed option is skipped in
    /// BOTH directions — it neither gains a tick nor decides whether "all on" is already true.
    /// Before this held, Ctrl+A could tick a box no key could reach to untick.
    #[test]
    fn select_all_cannot_touch_what_the_machine_ruled_out() {
        let mut form = Form::new()
            .checkboxes("apps", &["i3", "hyprland"])
            .incompatible(&[(&["wayland-only"], "this session is X11")]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!("boxes") };
        options[1].tags = vec!["wayland-only".into()];

        let rows = focusables(&form);
        let mut focus = 0;
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!("boxes") };
        assert!(options[0].checked, "select-all ticked what it could");
        assert!(!options[1].checked, "and left the ruled-out box exactly as it stood");

        // The greyed box must not count towards "everything is on", or the second press would
        // read one un-tickable hold-out as "not all on yet" and refuse to ever toggle off.
        apply(&mut form, &rows, &mut focus, Key::Home);
        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!("boxes") };
        assert!(!options[0].checked, "the second press toggles all OFF, greyed box or no");
    }

    /// Mirroring moves twins the user could have moved themselves, and nothing else: a
    /// rule-greyed twin is as locked as a disabled one always was.
    #[test]
    fn mirroring_skips_a_twin_the_machine_ruled_out() {
        let mut form = Form::new()
            .checkboxes("by cpu", &["wayvnc"])
            .checkboxes("by memory", &["wayvnc"])
            .mirror_duplicates()
            .incompatible(&[(&["wayland-only"], "this session is X11")]);
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!("boxes") };
        options[0].tags = vec!["wayland-only".into()];

        // Tick the live twin, the way a user would: find its row and press space.
        let rows = focusables(&form);
        let here = Focus::Option { item: 0, option: 0 };
        let mut focus = rows.iter().position(|row| *row == here).expect("a live row");
        apply(&mut form, &rows, &mut focus, Key::Char(' '));

        let Item::Checkboxes { options, .. } = &form.items[0] else { panic!("boxes") };
        assert!(options[0].checked, "the row that was pressed");
        let Item::Checkboxes { options, .. } = &form.items[1] else { panic!("boxes") };
        assert!(!options[0].checked, "its ruled-out twin stays put — not yours from any angle");
    }

    /// A ticked incompatible row keeps its tick. Someone who installed a Wayland thing and then
    /// logged into X11 has it installed, and a form that quietly cleared the box would be
    /// reporting a removal nobody asked for.
    #[test]
    fn greying_a_row_does_not_touch_what_it_already_says() {
        let form = sessions(true);
        assert!(form.chosen_at(0, 0), "the tick is a fact about the machine, not an offer");
        assert!(form.objections().is_empty(), "and greying alone blocks nothing");
    }

    /// Enter on Submit does nothing while an objection stands, and the objections are already
    /// drawn under the button — so the press that refuses is the press that points at why.
    #[test]
    fn submit_refuses_while_an_objection_stands_and_goes_once_it_is_answered() {
        let mut form = Form::new().grid(
            "",
            &["apt"],
            vec![
                GridRow::named("ncmpcpp").cells(vec![GridCell::set(None)]).requires(&["audio-backend"]),
                GridRow::named("mpd").cells(vec![GridCell::open(None)]).tags(&["audio-backend"]),
            ],
        );
        let rows = focusables(&form);
        let submit = rows.iter().position(|row| *row == Focus::Submit).expect("a submit row");

        let mut focus = submit;
        assert_eq!(
            apply(&mut form, &rows, &mut focus, Key::Enter),
            Action::Ignored,
            "the requirement is unmet, so the button does not go"
        );

        let drawn = render(&form, &rows, submit, 100, &["mind the gap".into()], &Opened::of(&form));
        let flat: Vec<&str> = drawn.iter().map(String::as_str).collect();
        let objection = flat.iter().position(|line| line.contains('\u{2717}')).expect("an objection");
        let caution = flat.iter().position(|line| line.contains('\u{26a0}')).expect("a caution");
        assert!(
            flat[objection].contains("nothing chosen is `audio-backend`"),
            "it says what is missing: {:?}",
            flat[objection]
        );
        assert!(objection < caution, "the stronger claim comes first: {flat:?}");

        // Tick the backend and the same key goes through — nothing else changed.
        let Item::Grid { rows: grid, .. } = &mut form.items[0] else { panic!("a grid") };
        grid[1].cells[0].checked = true;
        assert!(form.objections().is_empty());
        let rows = focusables(&form);
        let mut focus = rows.iter().position(|row| *row == Focus::Submit).expect("a submit row");
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Enter), Action::Submit);
    }

    /// Green marks the rows that would ANSWER what is missing, and stops the moment one is
    /// taken. It is the form pointing, exactly as blue is — see `WANTED_GREEN`.
    ///
    /// Asserted against a `console::style` expectation rather than by hunting for an escape
    /// sequence, because colour is suppressed when stdout is not a tty and a test looking for
    /// the code would simply never fire. Built the same way the drawing builds it, so the two
    /// agree under either setting — and the LABEL in each expectation is what makes it a real
    /// assertion when colour is off.
    #[test]
    fn the_rows_that_would_answer_a_requirement_are_drawn_green() {
        let build = |backend_on: bool| {
            Form::new().grid(
                "",
                &["apt"],
                vec![
                    GridRow::named("ncmpcpp").cells(vec![GridCell::set(None)]).requires(&["audio-backend"]),
                    GridRow::named("mpd")
                        .cells(vec![if backend_on {
                            GridCell::set(None)
                        } else {
                            GridCell::open(None)
                        }])
                        .tags(&["audio-backend"]),
                ],
            )
        };
        let row = |form: &Form, want: &str| {
            let rows = focusables(form);
            render(form, &rows, 0, 100, &[], &Opened::of(form))
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };

        let pointing = build(false);
        assert!(pointing.wanted_at(0, 1) && !pointing.wanted_at(0, 0));
        assert!(
            row(&pointing, "mpd").contains(&console::style("mpd").color256(WANTED_GREEN).to_string()),
            "the row that would answer it: {:?}",
            row(&pointing, "mpd")
        );

        let answered = build(true);
        assert!(!answered.wanted_at(0, 1), "answered, so the form stops pointing");
        assert!(
            !row(&answered, "mpd").contains(&format!("\u{1b}[38;5;{WANTED_GREEN}m")),
            "and the colour goes with it"
        );
    }

    // ——— fold rows are stops, and the screen follows the cursor ————————————

    /// The bug as it was met: in a folding grid, ↑/↓ stepped from cell to cell and straight over
    /// the fold rows between them, so no key could reach one and nothing in a grid could be
    /// folded. A title is a stop now, shut or open; opening lands inside, and the `^` is skipped.
    #[test]
    fn arrows_in_a_folding_grid_stop_on_titles_and_skip_closing_lines() {
        let mut form = Form::new()
            .collapsible()
            .grid(
                "",
                &["apt"],
                vec![
                    GridRow::named("a1").heading("one").cells(vec![GridCell::open(None)]),
                    GridRow::named("a2").cells(vec![GridCell::open(None)]),
                    GridRow::named("b1").heading("two").cells(vec![GridCell::open(None)]),
                ],
            )
            .folded(0, 2);
        let rows = focusables(&form);
        let mut focus = rows.iter().position(|row| *row == Focus::Cell { item: 0, row: 1, column: 0 }).unwrap();

        apply(&mut form, &rows, &mut focus, Key::ArrowDown);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 2, foot: false }, "the `>` of shut `two`");
        apply(&mut form, &rows, &mut focus, Key::ArrowRight);
        assert!(!form.collapsed.contains(&(0, 2)), "right opens it");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Cell { item: 0, row: 2, column: 0 }, "and lands on b1, not the title");
        apply(&mut form, &rows, &mut focus, Key::ArrowUp);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 2, foot: false }, "back onto the open title");
        apply(&mut form, &rows, &mut focus, Key::ArrowUp);
        assert_eq!(rows[focus], Focus::Cell { item: 0, row: 1, column: 0 }, "over `one`'s `^`, onto a2");
        assert!(!rows.iter().any(|row| matches!(row, Focus::Section { foot: true, .. })), "no `^` is a stop");
    }

    /// Moving between rows still lands in the nearest live column — the walk changed, the
    /// landing rule did not.
    #[test]
    fn moving_between_grid_rows_still_keeps_the_column() {
        let form = grid_form();
        let rows = focusables(&form);
        // brave's live cells are flatpak and snap; from firefox/snap, up should land on brave/snap.
        let from = rows
            .iter()
            .position(|row| *row == Focus::Cell { item: 0, row: 2, column: 2 })
            .expect("firefox via snap");
        let landed = _across_rows(&rows, from, false).expect("a row above");
        assert_eq!(rows[landed], Focus::Cell { item: 0, row: 1, column: 2 }, "same column, row above");
        // From brave/flatpak up to git, whose only live cell is apt: nearest wins.
        let from = rows
            .iter()
            .position(|row| *row == Focus::Cell { item: 0, row: 1, column: 1 })
            .expect("brave via flatpak");
        let landed = _across_rows(&rows, from, false).expect("a row above");
        assert_eq!(rows[landed], Focus::Cell { item: 0, row: 0, column: 0 });
    }

    /// A frame of thirty lines with a four-line footer, on a twelve-line screen: what shows.
    fn tall(focused: usize) -> Frame {
        Frame { lines: (0..30).map(|at| format!("line {at}")).collect(), focused, foot: 26 }
    }

    /// The footer is pinned and the body scrolls only as far as it must — still while the cursor
    /// moves inside the window, one row when it reaches an edge, and not at all while the cursor
    /// is in the footer. Never more lines than the screen has.
    #[test]
    fn the_viewport_follows_the_cursor_and_pins_the_footer() {
        let footer: Vec<String> = tall(0).lines[26..].to_vec();

        let (shown, scroll) = viewport(&tall(0), 12, 0);
        assert_eq!(shown.len(), 12);
        assert_eq!(shown[..8], tall(0).lines[..8], "the top of the body");
        assert_eq!(shown[8..], footer, "and the footer, whatever else shows");
        assert_eq!(scroll, 0);

        let (shown, scroll) = viewport(&tall(9), 12, 0);
        assert_eq!(scroll, 2, "one past the window: slid just far enough");
        assert!(shown.contains(&"line 9".to_string()) && shown[8..] == footer);

        let (_, scroll) = viewport(&tall(5), 12, 2);
        assert_eq!(scroll, 2, "inside the window: the picture holds still");
        let (_, scroll) = viewport(&tall(1), 12, 2);
        assert_eq!(scroll, 1, "above the window: slid back up");

        let (shown, scroll) = viewport(&tall(27), 12, 5);
        assert_eq!(scroll, 5, "the cursor is on Submit, and the body stays where it was");
        assert!(shown.contains(&"line 27".to_string()), "Submit is pinned, so it is on screen");

        // A remembered scroll deeper than the body allows is pulled back so no blank rows show.
        let (shown, scroll) = viewport(&tall(27), 12, 25);
        assert_eq!(scroll, 18);
        assert_eq!(shown[..8], tall(0).lines[18..26]);
    }

    /// Short terminals give up pinning rather than give up the body: below three lines of room the
    /// whole frame scrolls as one and the cursor is still on screen. A frame that fits at all is
    /// shown whole, wherever the cursor is.
    #[test]
    fn a_short_terminal_scrolls_the_whole_frame_and_a_fitting_one_never_scrolls() {
        let (shown, scroll) = viewport(&tall(15), 6, 0);
        assert_eq!(shown.len(), 6);
        assert_eq!(scroll, 10, "15 is the last line of a six-line window starting at 10");
        assert_eq!(shown, tall(0).lines[10..16], "no footer pinned: it would have been the screen");

        let (shown, scroll) = viewport(&tall(29), 40, 7);
        assert_eq!(shown, tall(0).lines, "it fits, so all of it");
        assert_eq!(scroll, 0, "and any old scroll is forgotten");
    }

    /// The two facts a frame reports beyond its lines, checked against a real drawing: the focused
    /// line is the one wearing the cursor, and the footer begins at the blank above Submit.
    #[test]
    fn a_frame_knows_its_focused_line_and_where_its_footer_starts() {
        let form = grid_form();
        let rows = focusables(&form);
        let opened = Opened::of(&form);

        let on_first = compose(&form, &rows, 0, 80, &[], &opened);
        let focused = console::strip_ansi_codes(&on_first.lines[on_first.focused]).to_string();
        assert!(on_first.lines[on_first.focused].contains(FOCUS_ON));
        assert!(focused.contains("git"), "the first live cell is git's: {focused:?}");
        assert_eq!(on_first.lines[on_first.foot], "", "the footer opens with the blank");
        assert!(on_first.lines[on_first.foot + 1].contains("[ Submit ]"));
        assert!(on_first.focused < on_first.foot, "a cell is in the body");

        let on_submit = compose(&form, &rows, rows.len() - 1, 80, &[], &opened);
        assert_eq!(on_submit.focused, on_submit.foot + 1, "Submit is the footer's second line");
    }

    // ——— fold titles look like filter boxes, and filter boxes count ————————

    /// A fold title is drawn in the filter block's violet, because it does the filter block's
    /// job: both decide what is on screen, and neither is an answer. Styled expectations, as
    /// every colour test here — see `the_filter_block_is_coloured_and_stays_coloured_under_the_cursor`.
    #[test]
    fn fold_titles_wear_the_filter_blocks_colour() {
        let violet = |text: &str| console::style(text).color256(FILTER_VIOLET).to_string();
        let form = Form::new()
            .collapsible()
            .grid(
                "",
                &["apt"],
                vec![
                    GridRow::named("a").heading("one").cells(vec![GridCell::open(None)]),
                    GridRow::named("b").heading("two").cells(vec![GridCell::open(None)]),
                ],
            )
            .folded(0, 1);
        let rows = focusables(&form);
        let lines = render(&form, &rows, rows.len() - 1, 80, &[], &Opened::of(&form));
        let line = |want: &str| {
            lines.iter().find(|line| console::strip_ansi_codes(line).trim() == want).expect("drawn").clone()
        };
        assert_eq!(line("v one"), format!("  {}", violet("v one")), "an open section's head");
        assert_eq!(line("^ one"), format!("  {}", violet("^ one")), "and its foot");
        assert_eq!(line("> two"), format!("  {}", violet("> two")), "a shut one");
        assert_eq!(line("[ ]  a").trim_start(), "[ ]  a", "the entries themselves stay plain");
    }

    /// The counts stand in a column of their own — labels padded to the widest, counts
    /// right-aligned to the widest — so `(15)` and `( 3)` end on the same character.
    #[test]
    fn filter_counts_line_up_whatever_their_width() {
        let tagged = |name: String, tag: &str| {
            GridRow::named(name).cells(vec![GridCell::open(None)]).tags(&[tag])
        };
        let rows: Vec<GridRow> = (0..15)
            .map(|at| tagged(format!("l{at}"), "legacy"))
            .chain((0..3).map(|at| tagged(format!("s{at}"), "spyware")))
            .collect();
        let form = Form::new()
            .grid("", &["apt"], rows)
            .filters("Include", &[(&["legacy"][..], "legacy"), (&["spyware"][..], "privacy")]);
        let drawn: Vec<String> = drawn(&form)
            .into_iter()
            .filter(|line| line.contains("[x]"))
            .map(|line| line.trim().to_string())
            .collect();
        assert_eq!(drawn, ["[x] legacy  (15)", "[x] privacy ( 3)"]);
    }

    // ——— ctrl+s and tab ———————————————————————————————————————————————————

    /// Ctrl+S is Submit from wherever the cursor is, and obeys the same refusal: an objection
    /// standing means the key does nothing, exactly as Enter on the dimmed button does.
    #[test]
    fn ctrl_s_submits_from_anywhere_unless_an_objection_stands() {
        let mut form = Form::new().grid(
            "",
            &["apt"],
            vec![
                GridRow::named("ncmpcpp").cells(vec![GridCell::set(None)]).requires(&["audio-backend"]),
                GridRow::named("mpd").cells(vec![GridCell::open(None)]).tags(&["audio-backend"]),
            ],
        );
        let rows = focusables(&form);
        let mut focus = 0; // the first cell, nowhere near Submit
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char(CTRL_S)), Action::Ignored);
        assert_eq!(focus, 0, "refused, and the cursor did not move: the objections are pinned anyway");

        let Item::Grid { rows: grid, .. } = &mut form.items[0] else { panic!("a grid") };
        grid[1].cells[0].checked = true;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char(CTRL_S)), Action::Submit);

        // And in a text field it submits rather than typing a control character into the answer.
        let mut form = Form::new().text("Name", "");
        let rows = focusables(&form);
        let mut focus = 0;
        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Char(CTRL_S)), Action::Submit);
        assert_eq!(form.text_value("Name"), Some(""), "nothing was typed");
    }

    /// Tab folds the section the cursor is IN — from a cell deep inside it, not only from its
    /// title — and lands on its `>`; Tab again from there unfolds it and lands on the first entry
    /// inside. Outside any section, or on a form that does not fold, it does nothing at all, and in
    /// particular no longer moves down.
    #[test]
    fn tab_folds_the_current_block_and_unfolds_it_again() {
        let mut form = Form::new().collapsible().grid(
            "",
            &["apt"],
            vec![
                GridRow::named("a1").heading("one").cells(vec![GridCell::open(None)]),
                GridRow::named("a2").cells(vec![GridCell::open(None)]),
                GridRow::named("b1").heading("two").cells(vec![GridCell::open(None)]),
            ],
        );
        let rows = focusables(&form);
        let mut focus = rows.iter().position(|row| *row == Focus::Cell { item: 0, row: 1, column: 0 }).unwrap();

        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Tab), Action::Redraw);
        assert!(form.collapsed.contains(&(0, 0)), "`one` is shut from inside it");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Section { item: 0, slot: 0, foot: false }, "resting on its head");

        assert_eq!(apply(&mut form, &rows, &mut focus, Key::Tab), Action::Redraw);
        assert!(!form.collapsed.contains(&(0, 0)), "and open again from the `>`");
        let rows = focusables(&form);
        assert_eq!(rows[focus], Focus::Cell { item: 0, row: 0, column: 0 }, "landing on the first entry");

        // Nothing to fold from the button.
        let submit = rows.iter().position(|row| *row == Focus::Submit).unwrap();
        let mut at_submit = submit;
        assert_eq!(apply(&mut form, &rows, &mut at_submit, Key::Tab), Action::Ignored);
        assert_eq!(at_submit, submit, "and the cursor stays: Tab is not ↓ any more");

        // Nor on a form that does not fold — where it used to be ↓, and is now nothing.
        let mut plain = Form::new().checkboxes("Tops", &["a", "b"]);
        let rows = focusables(&plain);
        let mut focus = 0;
        assert_eq!(apply(&mut plain, &rows, &mut focus, Key::Tab), Action::Ignored);
        assert_eq!(focus, 0);
    }

    /// The legend promises only what the form answers to: the fold key appears on a folding form
    /// and nowhere else. Ctrl+S is always there, because Submit always is.
    #[test]
    fn the_legend_offers_tab_only_where_there_is_something_to_fold() {
        let last = |form: &Form| drawn(form).last().cloned().unwrap_or_default();
        let plain = Form::new().checkboxes("Tops", &["a"]);
        assert!(last(&plain).contains("ctrl+s submit") && !last(&plain).contains("tab fold/unfold"));
        let folding = Form::new().collapsible().checkboxes("Tops", &["a"]);
        assert!(last(&folding).contains("ctrl+s submit · tab fold/unfold"), "{}", last(&folding));
    }

    /// Where the column names go in a folding grid: under every open title, and NOT at the top —
    /// a header there would head nothing but the first title. All shut, no names at all; open one,
    /// its copy appears. The one exception is loose rows before the first title, which have no
    /// copy to read from and so keep the top header.
    #[test]
    fn a_folding_grid_heads_each_open_block_instead_of_the_whole() {
        let names = |form: &Form| {
            drawn(form).into_iter().filter(|line| line.trim() == "apt  flatpak").count()
        };
        let cell = || vec![GridCell::open(None), GridCell::open(None)];
        let titled = || {
            vec![
                GridRow::named("a").heading("one").cells(cell()),
                GridRow::named("b").heading("two").cells(cell()),
            ]
        };
        let all_shut = Form::new().collapsible().grid("", &["apt", "flatpak"], titled()).all_folded();
        assert_eq!(names(&all_shut), 0, "nothing open, so nothing to head");
        let one_open = Form::new().collapsible().grid("", &["apt", "flatpak"], titled()).folded(0, 1);
        assert_eq!(names(&one_open), 1, "the open block's copy, and no header above it");
        let both_open = Form::new().collapsible().grid("", &["apt", "flatpak"], titled());
        assert_eq!(names(&both_open), 2);

        // Loose rows first: they have no title to carry a copy, so the top header stays for them.
        let loose = Form::new().collapsible().grid(
            "",
            &["apt", "flatpak"],
            vec![GridRow::named("loose").cells(cell()), GridRow::named("a").heading("one").cells(cell())],
        );
        let lines = drawn(&loose);
        assert_eq!(lines[0].trim(), "apt  flatpak", "the top header, for the loose row: {lines:#?}");
        assert_eq!(names(&loose), 2, "…plus the open block's copy");

        // And a grid that does not fold is exactly as it always was: one header, no copies.
        let plain = Form::new().grid("", &["apt", "flatpak"], titled());
        assert_eq!(names(&plain), 1);
    }

    // ——— the given glyph: the machine's ticks against the user's ————————————

    /// Three ticks, three glyphs: a box that arrived ticked is `[■]`, a box the user ticks is
    /// `[x]`, and a suggestion is `[x]` too — told apart by colour, since it asserts nothing.
    /// Clearing a fact reddens the empty box; ticking it again brings the given glyph back, not an `x`,
    /// because "as it was when we started" is true again.
    #[test]
    fn a_box_that_arrived_ticked_wears_the_given_glyph_and_keeps_it_when_re_ticked() {
        // Every glyph here is one cell wide, or the columns come apart. Pinned because the given
        // glyph has changed twice and both earlier choices were of ambiguous width.
        for glyph in [GIVEN, TICKED, CLEAR] {
            assert_eq!(console::measure_text_width(glyph), 3, "{glyph:?} is not three cells");
        }
        let mut form = Form::new().checkboxes("packages", &["installed", "wanted", "recommended"]);
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
        options[2].checked = true;
        options[2].suggested = true;
        let opened = Opened::of(&form);
        let rows = focusables(&form);
        let row = |form: &Form, want: &str| {
            render(form, &rows, rows.len() - 1, 80, &[], &opened)
                .into_iter()
                .find(|line| console::strip_ansi_codes(line).contains(want))
                .expect("drawn")
        };

        assert_eq!(row(&form, "installed"), "  [■] installed", "a fact: the given glyph");
        assert_eq!(row(&form, "wanted"), "  [ ] wanted");
        assert_eq!(
            row(&form, "recommended"),
            format!("  {}", console::style("[x] recommended").color256(SUGGESTED_BLUE)),
            "a suggestion keeps the x — its colour is what tells it apart"
        );

        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[1].checked = true;
        options[0].checked = false;
        assert_eq!(row(&form, "wanted"), "  [x] wanted", "the user's own tick is an x");
        assert_eq!(
            row(&form, "installed"),
            console::style("  [ ] installed").red().to_string(),
            "a fact undone"
        );

        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
        assert_eq!(row(&form, "installed"), "  [■] installed", "re-ticked: the given glyph, not an x");

        // Grids say it the same way, cell by cell.
        let grid = Form::new().grid(
            "",
            &["apt", "flatpak"],
            vec![GridRow::named("brave").cells(vec![GridCell::set(None), GridCell::open(None).suggest()])],
        );
        let rows = focusables(&grid);
        let line = render(&grid, &rows, 0, 80, &[], &Opened::of(&grid))
            .into_iter()
            .map(|line| console::strip_ansi_codes(&line).into_owned())
            .find(|line| line.contains("brave"))
            .expect("drawn");
        assert!(line.contains("[■]") && line.contains("[x]"), "a fact and a suggestion: {line:?}");
    }
}
