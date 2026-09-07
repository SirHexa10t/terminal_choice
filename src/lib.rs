//! Interactive terminal forms: put a form on the terminal, hand back what the user chose.
//!
//! A form is a sequence of [`Item`]s — checkbox groups (choose many), radio groups (choose one),
//! free-text fields, and comments, which are display-only: the cursor skips them and nothing can
//! change them. Within a choice group, options may carry sub-titles of their own, and may be
//! fixed — shown and counted, but not the user's to change. Build one in code (the builder methods on [`Form`]), from CLI-style args
//! ([`Form::from_args`]), or from a TOML file ([`Form::from_toml`]); then [`run`] it, and read
//! the answers back through the accessors or as one TOML document ([`Form::answers_toml`]).
//!
//! The interactive part is deliberately thin: rendering and key handling are pure functions in
//! [`ui`], tested without any terminal, and [`run`] is the small loop that connects them to a
//! real one.
//!
//! A form can also show [`Warning`]s: a line of red underneath, for as long as some answer is
//! worth remarking on. What counts as worth remarking on is the CALLER's to decide — "two VPN
//! clients at once", "that path already exists" are facts about someone's domain, not about
//! forms — so there are two ways in, and a run shows both:
//!
//! - **A closure**, via [`run_with_warnings`]. Bound by nothing: it may consult the machine, the
//!   network, the caller's own tables. This is the general door.
//! - **The definition itself**, via [`Form::warning`] or a `[[warning]]` block in the TOML. A
//!   file cannot carry a closure, so these are written in the fixed vocabulary of [`Condition`]
//!   — enough for "these were ticked together", which is most of them.
//!
//! Warnings never gate anything: an odd set of answers is still an answer, and every one of them
//! can be submitted while every warning shows.
//!
//! One thing a run marks on its own, needing nothing from the caller: a checkbox that the form
//! ARRIVED with ticked and the user has since cleared is drawn red. Only that direction — a form
//! that opens empty and gets filled in would otherwise mark every answer it was given — because
//! clearing a tick the form asserted reads as undoing a fact, and is worth seeing before submit.

mod comments;
mod definition;
pub mod prompts;
pub mod ui;

pub use prompts::{prompt_Yn, prompt_yN, prompt_yn}; // capitals mirror the [Y/n]/[y/N] each prints
pub use ui::{run, run_with_warnings, Outcome};

/// One entry of a form, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Display-only text: rendered dim, never focusable, never in the answers.
    Comment(String),
    /// Choose any number of `options`; `checked`, `headings` and `enabled` all run parallel
    /// to them (see [`Item::Radio`] for what the last two mean — they are the same here).
    Checkboxes {
        label: String,
        options: Vec<String>,
        checked: Vec<bool>,
        headings: Vec<Option<String>>,
        enabled: Vec<bool>,
    },
    /// Choose exactly one of `options` (or none, if the user never picks).
    ///
    /// `headings[i]`, when present, is a sub-title drawn above option `i` — what a long list
    /// needs to read as sections ("dev-tools", "web browsers") without becoming several groups
    /// and several answers. Multi-line headings draw on several lines.
    ///
    /// `enabled[i]` is whether option `i` is the user's to change. A disabled option is drawn
    /// dim and the cursor never lands on it, so no key can reach it — but it keeps whatever
    /// state it was given and still counts in the answers: "already true, not yours to change".
    Radio {
        label: String,
        options: Vec<String>,
        chosen: Option<usize>,
        headings: Vec<Option<String>>,
        enabled: Vec<bool>,
    },
    /// Free text, editable in place.
    Text { label: String, value: String },
    /// A table of checkboxes: one row per thing, one column per way of having it.
    ///
    /// For the choice that is not "which of these" but "which of these, HOW" — a package and the
    /// managers that carry it, a file and the machines to write it to. A row of independent
    /// checkbox groups could hold the same answers, but not the same question: the point is that
    /// the columns line up down the page, so a column can be read as a column.
    Grid {
        label: String,
        /// Column headings, left to right. Drawn truncated to the width of a box, since the
        /// focused cell's own line says which column it is in full.
        columns: Vec<String>,
        rows: Vec<GridRow>,
    },
}

/// One row of an [`Item::Grid`]: a thing, and one cell per column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    /// Named after the boxes rather than before them, so the boxes of every row start in the
    /// same place and the columns read straight down.
    pub label: String,
    /// A sub-title drawn above this row — the "# category" line that breaks a long table into
    /// sections. Multi-line headings draw on several lines.
    pub heading: Option<String>,
    /// A remark drawn after the label, dim, aligned into a column of its own — what the thing IS,
    /// where the name alone does not say. Display only: [`GridRow::label`] stays the answer.
    pub note: Option<String>,
    /// Parallel to the grid's `columns`. A row with fewer cells than there are columns simply
    /// has nothing to say about the rest, which draws as blank.
    pub cells: Vec<GridCell>,
}

/// One box of an [`Item::Grid`], and what it would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridCell {
    pub checked: bool,
    /// Whether this cell is the user's to change. A disabled cell is drawn dim and the cursor
    /// never lands on it.
    pub enabled: bool,
    /// Whether there is a box here at all.
    ///
    /// The distinction a disabled box cannot make on its own, and the one that decides whether a
    /// reader has anything to think about. A dim `[ ]` says "this column COULD serve this row,
    /// but not as things stand" — which is a prompt: make it so, and this becomes available. A
    /// gap says "this column can never serve this row", and asks nothing of anybody.
    ///
    /// Draw a gap only for the second. Drawing one for the first hides an option behind a
    /// character that means "nothing here".
    pub boxed: bool,
    /// What ticking this would do, shown under the form while the cursor is on it and it is
    /// CLEAR. The two are separate because a box's meaning depends on which way it is about to
    /// move: over an empty box the interesting line is the one that fills it.
    pub on_set: Option<String>,
    /// What clearing this would do, shown while the cursor is on it and it is TICKED.
    pub on_clear: Option<String>,
}

impl GridCell {
    /// A cell the user may tick, doing `on_set`.
    #[must_use]
    pub fn open(on_set: Option<String>) -> Self {
        Self { checked: false, enabled: true, boxed: true, on_set, on_clear: None }
    }

    /// A cell that arrives ticked, and would do `on_clear` if emptied.
    #[must_use]
    pub fn set(on_clear: Option<String>) -> Self {
        Self { checked: true, enabled: true, boxed: true, on_set: None, on_clear }
    }

    /// A box that is shown but not the user's to change — dim, and space does nothing on it.
    ///
    /// Still a BOX, and still somewhere the cursor can rest: a reader deciding what to go and
    /// set up needs to see the choice that is out of reach, and `would` is what it would run
    /// once it were not. That line under the cursor is the whole argument for installing
    /// whatever this column needs.
    #[must_use]
    pub fn locked(checked: bool, would: Option<String>) -> Self {
        match checked {
            true => Self { checked, enabled: false, boxed: true, on_set: None, on_clear: would },
            false => Self { checked, enabled: false, boxed: true, on_set: would, on_clear: None },
        }
    }

    /// No box: this column can never serve this row. Draws as a gap.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }
}

/// One caution a form shows about the answers currently in it: what to say, and the
/// [`Condition`] that makes it worth saying.
///
/// A warning is not validation. It never blocks a submit and never changes an answer — some
/// combinations are merely odd enough to be worth naming before the user commits to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub text: String,
    pub when: Condition,
}

/// A condition a [`Warning`] can be written in when its definition lives in a FILE.
///
/// This is a SERIALIZATION vocabulary, not the warning mechanism. A TOML document cannot carry a
/// closure, so the conditions a definition file can state have to be a fixed, named set — and a
/// fixed set can only ever cover the cases one author imagined. Anything outside it (a fact about
/// the machine, a regex over a text field, a rule consulting something this crate has never heard
/// of) goes in the closure [`run_with_warnings`] takes, which is bound by nothing here. The two
/// compose: a run shows the form's own warnings AND whatever the closure adds.
///
/// Every leaf names its group by label and is FALSE when no such group exists. That asymmetry is
/// deliberate: a rule aimed at a field that was never added stays quiet, instead of `Unchecked`
/// finding nothing checked and firing on every form that lacks the field entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Every one of `options` is checked, in the checkbox group `label`.
    Checked { label: String, options: Vec<String> },
    /// None of `options` is checked, in the checkbox group `label`.
    Unchecked { label: String, options: Vec<String> },
    /// At least `count` of `options` are checked — "two of these at once is probably an
    /// accident", the shape most warnings about a choose-many group turn out to have.
    CheckedAtLeast { label: String, options: Vec<String>, count: usize },
    /// The radio group `label` is sitting on `option`.
    Chosen { label: String, option: String },
    /// All of these hold. Empty holds: nothing contradicts it.
    All(Vec<Condition>),
    /// Any of these holds. Empty does NOT hold: nothing supports it.
    Any(Vec<Condition>),
    /// This one does not hold.
    Not(Box<Condition>),
}

impl Condition {
    /// Whether `form`'s answers, as they stand, satisfy this condition.
    #[must_use]
    pub fn holds(&self, form: &Form) -> bool {
        match self {
            Self::Checked { label, options } => {
                matches!(form._checked_among(label, options), Some(hits) if hits == options.len())
            }
            Self::Unchecked { label, options } => {
                matches!(form._checked_among(label, options), Some(0))
            }
            Self::CheckedAtLeast { label, options, count } => {
                matches!(form._checked_among(label, options), Some(hits) if hits >= *count)
            }
            Self::Chosen { label, option } => form.chosen(label) == Some(option.as_str()),
            Self::All(of) => of.iter().all(|one| one.holds(form)),
            Self::Any(of) => of.iter().any(|one| one.holds(form)),
            Self::Not(one) => !one.holds(form),
        }
    }
}

/// A form: an optional title and the items, in the order they show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub title: Option<String>,
    pub items: Vec<Item>,
    /// Cautions about the answers, shown under the form while their conditions hold — the ones a
    /// DEFINITION can state (see [`Condition`]). A caller with richer rules adds them at run
    /// time instead, via [`run_with_warnings`]; both are shown.
    pub warnings: Vec<Warning>,
    /// Pad comment lines by the width of a checkbox prefix, so a comment and an option beneath
    /// each other keep their columns aligned — for forms whose comments and options interleave
    /// into one table (a process tree, say).
    pub aligned: bool,
    /// Strip ANSI colour codes out of everything displayed. Coloured option text is welcome by
    /// default (a caller may highlight matched substrings, `grep`-style) — this is the opt-out
    /// for answers that must come back as clean text.
    pub scrub_colors: bool,
    /// Treat identically-worded checkbox options as THE SAME ENTRY wherever they appear:
    /// ticking one ticks its twins in every other group (and unticking likewise). For forms
    /// that show one thing under several headings — a process in both a "top CPU" and a "top
    /// memory" list — where diverging states would be a contradiction.
    pub mirror_duplicates: bool,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    // ——— building in code ———————————————————————————————————————————————

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Display-only text between fields — instructions, section headers, caveats.
    pub fn comment(mut self, text: impl Into<String>) -> Self {
        self.items.push(Item::Comment(text.into()));
        self
    }

    /// A choose-many group: all boxes clear, all selectable, no sub-titles.
    ///
    /// Sub-titles and disabling are per-option and rarer than the group itself, so they are set
    /// on the fields afterwards rather than through a builder that every caller would pass
    /// nothing to — `Item`'s fields are public for exactly this.
    pub fn checkboxes(mut self, label: impl Into<String>, options: &[&str]) -> Self {
        self.items.push(Item::Checkboxes {
            label: label.into(),
            checked: vec![false; options.len()],
            headings: vec![None; options.len()],
            enabled: vec![true; options.len()],
            options: options.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// A choose-one group: nothing chosen, all selectable, no sub-titles.
    pub fn radio(mut self, label: impl Into<String>, options: &[&str]) -> Self {
        self.items.push(Item::Radio {
            label: label.into(),
            chosen: None,
            headings: vec![None; options.len()],
            enabled: vec![true; options.len()],
            options: options.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// A table of checkboxes — see [`Item::Grid`]. Rows are built by the caller, since what a
    /// cell offers is entirely the caller's business.
    pub fn grid(mut self, label: impl Into<String>, columns: &[&str], rows: Vec<GridRow>) -> Self {
        self.items.push(Item::Grid {
            label: label.into(),
            columns: columns.iter().map(|column| (*column).to_string()).collect(),
            rows,
        });
        self
    }

    /// A caution to show whenever `when` holds (see [`Warning`]). Order is display order.
    pub fn warning(mut self, text: impl Into<String>, when: Condition) -> Self {
        self.warnings.push(Warning { text: text.into(), when });
        self
    }

    /// A free-text field, optionally pre-filled.
    pub fn text(mut self, label: impl Into<String>, prefill: impl Into<String>) -> Self {
        self.items.push(Item::Text { label: label.into(), value: prefill.into() });
        self
    }

    /// Align interleaved comments with checkbox options (see the `aligned` field).
    pub fn aligned(mut self) -> Self {
        self.aligned = true;
        self
    }

    /// Link identically-worded checkbox options across the form (see `mirror_duplicates`).
    pub fn mirror_duplicates(mut self) -> Self {
        self.mirror_duplicates = true;
        self
    }

    /// Strip colour codes from everything displayed (see `scrub_colors`).
    pub fn scrub_colors(mut self) -> Self {
        self.scrub_colors = true;
        self
    }

    // ——— building from a definition ————————————————————————————————————

    /// Load a form from TOML text. The schema, in full:
    ///
    /// ```toml
    /// title = "Optional title"
    ///
    /// [[item]]
    /// kind = "comment"
    /// text = "Display-only line."
    ///
    /// [[item]]
    /// kind = "checkboxes"
    /// label = "Toppings"
    /// options = ["olives", "onion"]
    /// checked = ["onion"]          # optional pre-selection
    ///
    /// # A comment on its own line is kept, WHERE IT STANDS: above the item it precedes, or —
    /// # inside an options array — as a sub-title above the option it precedes. Which is how a
    /// # long list reads as sections without becoming several groups and several answers.
    /// [[item]]
    /// kind = "checkboxes"
    /// label = "Packages"
    /// options = [
    ///     # dev-tools
    ///     "zed",
    ///     "helix",
    ///     # security
    ///     # (consecutive lines are one sub-title)
    ///     { name = "mullvad", check_if = "path:mullvad", enabled_if = "path:apt" },
    /// ]
    ///
    /// [[item]]
    /// kind = "radio"
    /// label = "Size"
    /// options = ["S", "M", "L"]
    /// chosen = "M"                 # optional pre-selection
    ///
    /// [[item]]
    /// kind = "text"
    /// label = "Name"
    /// value = "optional prefill"
    ///
    /// # Cautions, shown while their condition holds. Optional, any number. Each file states its
    /// # own rules and its own wording — nothing about them is known to this crate in advance.
    /// [[warning]]
    /// text = "Olives and pineapple together — unusual, but allowed."
    /// when = { kind = "checked", label = "Toppings", options = ["olives", "pineapple"] }
    ///
    /// [[warning]]
    /// text = "More than one of these is probably an accident."
    /// when = { kind = "checked_at_least", label = "Toppings", options = ["olives", "onion"], count = 2 }
    /// ```
    ///
    /// The condition kinds are `checked`, `unchecked` and `checked_at_least` (each taking
    /// `label` + `options`, the last also `count`), `chosen` (`label` + `option`, for a radio),
    /// and the combinators `all`, `any` and `not`, which nest under `of`:
    ///
    /// ```toml
    /// [[warning]]
    /// text = "A container runtime was picked, but no CLI to drive it."
    /// when = { kind = "all", of = [
    ///     { kind = "checked",   label = "Runtime", options = ["docker"] },
    ///     { kind = "unchecked", label = "Tools",   options = ["docker-compose"] },
    /// ] }
    /// ```
    ///
    /// A rule naming a label or option the form does not have is refused here rather than
    /// silently never firing. For a rule this vocabulary cannot express, hand a closure to
    /// [`run_with_warnings`] instead — the two are shown together.
    ///
    /// ## Options that say more than their name
    ///
    /// An option is a bare string, or a table when it has more to say:
    ///
    /// - `name` — what it is called, and what the answers give back.
    /// - `check_if` — a predicate; when it holds, the box starts ticked.
    /// - `enabled_if` — a predicate; when it does NOT hold, the box is drawn dim and the cursor
    ///   skips it, so no key can change it. It keeps its state and still counts in the answers.
    ///
    /// The predicates are opaque strings: this crate never interprets `"path:zed"`, it hands it
    /// to whoever loaded the form. Use [`Form::from_toml_with`] to supply that answer; through
    /// this door nobody answers, so no box is pre-ticked and every box stays usable.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        definition::from_toml(text, None)
    }

    /// [`Form::from_toml`], with somebody to answer the file's `check_if` / `enabled_if`
    /// predicates.
    ///
    /// Those are opaque strings here: `"path:zed"` means nothing to this crate, and `resolve`
    /// says whether it is true of the machine in front of it. Same division as warnings — a form
    /// library cannot know what makes a box worth pre-ticking on someone else's system, so it
    /// carries the question and the caller answers it.
    ///
    /// [`Form::from_toml`] is this with nobody answering, and the two unanswered defaults differ
    /// deliberately: an unanswered `check_if` leaves its box clear, an unanswered `enabled_if`
    /// leaves its box usable. A form nobody can touch would be the worse failure.
    ///
    /// ```
    /// # use terminal_choice::{Form, Item};
    /// let text = r#"
    ///     [[item]]
    ///     kind = "checkboxes"
    ///     label = "Packages"
    ///     options = [
    ///         { name = "zed", check_if = "installed:zed" },
    ///         { name = "apt", enabled_if = "never" },
    ///     ]
    /// "#;
    /// let form = Form::from_toml_with(text, |spec| spec.starts_with("installed:")).unwrap();
    /// let Item::Checkboxes { checked, enabled, .. } = &form.items[0] else { panic!() };
    /// assert_eq!(checked, &[true, false], "the machine already has zed");
    /// assert_eq!(enabled, &[true, false], "and apt is not this user's to change");
    /// ```
    pub fn from_toml_with(text: &str, resolve: impl Fn(&str) -> bool) -> Result<Self, String> {
        definition::from_toml(text, Some(&resolve))
    }

    /// Build a form from CLI-style flags, in the order given:
    /// `--title T`, `--comment TEXT`, `--checkbox "Label: a, b, c"`, `--radio "Label: a, b, c"`,
    /// `--text "Label"`. A trailing `= …` pre-selects: `--radio "Size: S, M, L = M"`,
    /// `--checkbox "Tops: a, b = a, b"`, `--text "Name = prefill"`.
    pub fn from_args<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        definition::from_args(args.into_iter().map(Into::into))
    }

    // ——— reading the answers ————————————————————————————————————————————

    /// The checked options of the checkbox group called `label`.
    pub fn checked(&self, label: &str) -> Vec<&str> {
        self.items
            .iter()
            .find_map(|item| match item {
                Item::Checkboxes { label: l, options, checked, .. } if l == label => Some(
                    options
                        .iter()
                        .zip(checked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| option.as_str())
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// How many of `options` are checked in the group `label` — `None` when the form has no such
    /// checkbox group at all, which is the distinction that keeps [`Condition::Unchecked`] from
    /// being trivially true against a field that isn't there.
    fn _checked_among(&self, label: &str, options: &[String]) -> Option<usize> {
        self.items.iter().find_map(|item| match item {
            Item::Checkboxes { label: name, options: all, checked, .. } if name == label => Some(
                all.iter()
                    .zip(checked)
                    .filter(|(name, on)| **on && options.iter().any(|want| want == *name))
                    .count(),
            ),
            _ => None,
        })
    }

    /// The form's OWN warnings whose conditions hold right now, in definition order. A run also
    /// shows whatever [`run_with_warnings`]' closure adds; this is only the declared half.
    #[must_use]
    pub fn active_warnings(&self) -> Vec<&str> {
        self.warnings
            .iter()
            .filter(|warning| warning.when.holds(self))
            .map(|warning| warning.text.as_str())
            .collect()
    }

    /// The chosen option of the radio group called `label`, if one was picked.
    pub fn chosen(&self, label: &str) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            Item::Radio { label: l, options, chosen, .. } if l == label => {
                chosen.map(|index| options[index].as_str())
            }
            _ => None,
        })
    }

    /// The current value of the text field called `label`.
    pub fn text_value(&self, label: &str) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            Item::Text { label: l, value } if l == label => Some(value.as_str()),
            _ => None,
        })
    }

    /// Every answer as one TOML document, labels as keys: text fields as strings, radios as
    /// strings (omitted while unchosen), checkbox groups as arrays (present even when empty —
    /// "asked, none apply" is an answer). Comments contribute nothing, and neither does any
    /// item with an EMPTY label: an anonymous group is for embedding (its owner reads `items`
    /// directly), and "" as a key would collide the moment there were two.
    pub fn answers_toml(&self) -> String {
        let mut table = toml::value::Table::new();
        for item in &self.items {
            if matches!(item,
                Item::Checkboxes { label, .. } | Item::Radio { label, .. } | Item::Text { label, .. }
                    if label.is_empty())
            {
                continue;
            }
            match item {
                Item::Comment(_) => {}
                Item::Checkboxes { label, options, checked, .. } => {
                    let picked: Vec<toml::Value> = options
                        .iter()
                        .zip(checked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| toml::Value::String(option.clone()))
                        .collect();
                    table.insert(label.clone(), toml::Value::Array(picked));
                }
                Item::Radio { label, options, chosen, .. } => {
                    if let Some(index) = chosen {
                        table.insert(label.clone(), toml::Value::String(options[*index].clone()));
                    }
                }
                Item::Text { label, value } => {
                    table.insert(label.clone(), toml::Value::String(value.clone()));
                }
                // A grid answers in two dimensions, so it answers as a table of them: each row
                // that has anything ticked maps to the columns it ticked. A row with none is
                // omitted rather than written empty — unlike a checkbox group, where "asked,
                // none apply" is itself the answer, a blank grid row is simply not part of it.
                Item::Grid { label, columns, rows } => {
                    let mut picked = toml::value::Table::new();
                    for row in rows {
                        let ticked: Vec<toml::Value> = row
                            .cells
                            .iter()
                            .enumerate()
                            .filter(|(_, cell)| cell.checked)
                            .filter_map(|(at, _)| columns.get(at))
                            .map(|column| toml::Value::String(column.clone()))
                            .collect();
                        if !ticked.is_empty() {
                            picked.insert(row.label.clone(), toml::Value::Array(ticked));
                        }
                    }
                    table.insert(label.clone(), toml::Value::Table(picked));
                }
            }
        }
        // `to_string` is the DOCUMENT serializer. Display on a bare Value renders the inline
        // form (`{ key = … }`), which is a value, not a document — it doesn't even reparse.
        // A string-keyed string-valued table cannot fail to serialize.
        toml::to_string(&toml::Value::Table(table)).expect("plain tables always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Form {
        Form::new()
            .title("Order")
            .comment("Pick what you want.")
            .checkboxes("Toppings", &["olives", "onion", "feta"])
            .radio("Size", &["S", "M", "L"])
            .text("Name", "")
    }

    #[test]
    fn accessors_read_back_what_was_set() {
        let mut form = sample();
        let Item::Checkboxes { checked, .. } = &mut form.items[1] else { panic!() };
        checked[0] = true;
        checked[2] = true;
        let Item::Radio { chosen, .. } = &mut form.items[2] else { panic!() };
        *chosen = Some(1);
        let Item::Text { value, .. } = &mut form.items[3] else { panic!() };
        *value = "Ada".into();

        assert_eq!(form.checked("Toppings"), ["olives", "feta"]);
        assert_eq!(form.chosen("Size"), Some("M"));
        assert_eq!(form.text_value("Name"), Some("Ada"));
        // Asking for something that isn't there answers emptily rather than lying.
        assert!(form.checked("Size").is_empty(), "a radio is not a checkbox group");
        assert_eq!(form.chosen("Nope"), None);
    }

    /// The shape most declared warnings have: several options mean the same KIND of thing, and
    /// picking two of that kind at once is odd enough to say so — without forbidding it.
    #[test]
    fn a_count_threshold_catches_two_of_a_kind() {
        let vpns = ["mullvad", "wireguard", "proton"].map(String::from).to_vec();
        let mut form = Form::new()
            .checkboxes("packages", &["mullvad", "wireguard", "proton", "zed"])
            .warning(
                "More than one VPN client — unusual, but allowed.",
                Condition::CheckedAtLeast { label: "packages".into(), options: vpns, count: 2 },
            );
        let tick = |form: &mut Form, slots: &[usize]| {
            let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
            checked.iter_mut().for_each(|on| *on = false);
            slots.iter().for_each(|slot| checked[*slot] = true);
        };

        assert!(form.active_warnings().is_empty(), "nothing picked yet");
        tick(&mut form, &[0]);
        assert!(form.active_warnings().is_empty(), "one VPN is a normal choice");
        tick(&mut form, &[0, 3]);
        assert!(form.active_warnings().is_empty(), "an unlisted package does not count");
        tick(&mut form, &[0, 1]);
        assert_eq!(form.active_warnings().len(), 1, "two VPNs is the case being warned about");
        tick(&mut form, &[0, 1, 2]);
        assert_eq!(form.active_warnings().len(), 1, "three warns once, not three times");
    }

    /// Each leaf, and the rule that separates them: a condition against a group the form does not
    /// have is FALSE, never vacuously true — otherwise `Unchecked` would fire on every form that
    /// simply lacks the field, which is the loudest possible way to be useless.
    #[test]
    fn a_condition_against_a_missing_group_stays_quiet() {
        let mut form = Form::new().checkboxes("Tops", &["a", "b"]).radio("Size", &["S", "M"]);
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = true;
        let Item::Radio { chosen, .. } = &mut form.items[1] else { panic!() };
        *chosen = Some(1);

        let listed = |names: &[&str]| names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
        let on = |label: &str, names: &[&str]| Condition::Checked {
            label: label.into(),
            options: listed(names),
        };
        let off = |label: &str, names: &[&str]| Condition::Unchecked {
            label: label.into(),
            options: listed(names),
        };

        assert!(on("Tops", &["a"]).holds(&form));
        assert!(!on("Tops", &["a", "b"]).holds(&form), "`checked` means ALL of them");
        assert!(off("Tops", &["b"]).holds(&form));
        assert!(!off("Tops", &["a", "b"]).holds(&form), "one of them is checked");
        assert!(Condition::Chosen { label: "Size".into(), option: "M".into() }.holds(&form));
        assert!(!Condition::Chosen { label: "Size".into(), option: "S".into() }.holds(&form));

        // The asymmetry that matters: absent group, never true — not even for `unchecked`.
        assert!(!on("Nope", &["a"]).holds(&form));
        assert!(!off("Nope", &["a"]).holds(&form), "absent is not the same as empty");
        assert!(
            !Condition::Chosen { label: "Tops".into(), option: "a".into() }.holds(&form),
            "a checkbox group is not a radio"
        );
    }

    /// The combinators, including what the two empty cases mean — `All` of nothing holds (nothing
    /// contradicts it), `Any` of nothing does not (nothing supports it).
    #[test]
    fn combinators_nest_and_the_empty_cases_differ() {
        let mut form = Form::new().checkboxes("Tops", &["a", "b"]);
        let Item::Checkboxes { checked, .. } = &mut form.items[0] else { panic!() };
        checked[0] = true;
        let on = Condition::Checked { label: "Tops".into(), options: vec!["a".into()] };
        let off = Condition::Checked { label: "Tops".into(), options: vec!["b".into()] };

        assert!(Condition::All(vec![on.clone()]).holds(&form));
        assert!(!Condition::All(vec![on.clone(), off.clone()]).holds(&form));
        assert!(Condition::Any(vec![on.clone(), off.clone()]).holds(&form));
        assert!(!Condition::Any(vec![off.clone()]).holds(&form));
        assert!(Condition::Not(Box::new(off.clone())).holds(&form));
        assert!(Condition::All(vec![]).holds(&form), "nothing contradicts an empty All");
        assert!(!Condition::Any(vec![]).holds(&form), "nothing supports an empty Any");
        assert!(Condition::All(vec![on, Condition::Not(Box::new(off))]).holds(&form), "nests");
    }

    /// Warnings are commentary on the answers, not part of them.
    #[test]
    fn warnings_never_reach_the_answers() {
        let form =
            Form::new().checkboxes("Tops", &["a"]).warning("noisy", Condition::All(vec![]));
        assert_eq!(form.active_warnings(), ["noisy"], "an empty All always holds");
        assert!(!form.answers_toml().contains("noisy"), "{}", form.answers_toml());
    }

    #[test]
    fn answers_serialize_as_one_toml_document() {
        let mut form = sample();
        let Item::Checkboxes { checked, .. } = &mut form.items[1] else { panic!() };
        checked[1] = true;
        let Item::Text { value, .. } = &mut form.items[3] else { panic!() };
        *value = "Ada".into();

        let answers = form.answers_toml();
        assert!(answers.contains(r#"Toppings = ["onion"]"#), "{answers}");
        assert!(answers.contains(r#"Name = "Ada""#), "{answers}");
        assert!(!answers.contains("Size"), "an unchosen radio is omitted: {answers}");
        assert!(!answers.contains("Pick what"), "comments are not answers: {answers}");
        // The document must parse back — labels with spaces and quotes included.
        let form2 = Form::new().text("Full name", "O'Brien \"Bob\"");
        let parsed: toml::Value = toml::from_str(&form2.answers_toml()).expect("valid TOML out");
        assert_eq!(
            parsed.get("Full name").and_then(|v| v.as_str()),
            Some("O'Brien \"Bob\""),
            "keys and values survive quoting"
        );
    }
}
