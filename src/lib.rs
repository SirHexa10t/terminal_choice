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
    /// Choose any number of `options`. Each carries its own state — see [`Choice`].
    Checkboxes { label: String, options: Vec<Choice> },
    /// Choose exactly one of `options` (or none, if the user never picks).
    ///
    /// The selection is `chosen` rather than a flag on each option, because "exactly one" is a
    /// fact about the GROUP and per-option flags could contradict it. [`Choice::checked`] is
    /// therefore ignored here.
    Radio { label: String, chosen: Option<usize>, options: Vec<Choice> },
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

/// One box of the filter block: which entries it governs, and the words on it.
///
/// A rule rather than a tag, because the useful filters are not tags. "terminal-only" is not the
/// `terminal` tag — plenty of things carry both `terminal` and `gui` — it is `terminal` AND NOT
/// `gui`. A one-tag filter cannot say that, and a filter that cannot say it hides the wrong rows.
///
/// The shape is a conjunction: every tag in `all_of` must be present and every tag in `none_of`
/// must be absent. That covers what a filter row is actually asked to express, and stops short of
/// a general boolean language nobody would type into a `const`.
///
/// ```
/// # use terminal_choice::Rule;
/// let only = Rule::of("terminal-only", &["terminal", "!gui"]);
/// assert!(only.matches(&["terminal".into()]));
/// assert!(!only.matches(&["terminal".into(), "gui".into()]), "both means neither -only");
/// assert!(!only.matches(&["gui".into()]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The words on the box. Also its identity in [`Form::excluded`], so keep them distinct.
    pub label: String,
    /// Tags the entry must carry — all of them.
    pub all_of: Vec<String>,
    /// Tags the entry must not carry — any one of them disqualifies it.
    pub none_of: Vec<String>,
}

impl Rule {
    /// A rule from `label` and terms, where a leading `!` negates: `["terminal", "!gui"]`.
    ///
    /// The `!` is read HERE, once, rather than at every match — so the stored rule is already
    /// split into what must hold and what must not, and nothing downstream parses anything.
    #[must_use]
    pub fn of(label: impl Into<String>, terms: &[&str]) -> Self {
        let mut all_of = Vec::new();
        let mut none_of = Vec::new();
        for term in terms {
            match term.strip_prefix('!') {
                Some(tag) => none_of.push(tag.to_string()),
                None => all_of.push((*term).to_string()),
            }
        }
        Self { label: label.into(), all_of, none_of }
    }

    /// Whether an entry carrying `tags` is one this rule governs.
    ///
    /// A rule with no terms at all matches EVERYTHING, and clearing its box would empty the form.
    /// Not guarded against: it is a caller writing a filter that says nothing, and inventing a
    /// silent exception would hide the mistake rather than the rows.
    #[must_use]
    pub fn matches(&self, tags: &[String]) -> bool {
        self.all_of.iter().all(|want| tags.iter().any(|tag| tag == want))
            && !self.none_of.iter().any(|deny| tags.iter().any(|tag| tag == deny))
    }
}

/// One option of a choice group — [`Item::Checkboxes`] or [`Item::Radio`].
///
/// Replaces what were six arrays running alongside each other: `options`, `checked`, `headings`,
/// `enabled`, `tags`, `suggested`. Every one had to stay exactly as long as the rest, and nothing
/// enforced it — a group built with five states for six options was a panic waiting for the right
/// keystroke. One struct per option makes that class of mistake unrepresentable.
///
/// Built fluently, like `software_inventory`'s `Package`, because most options say only their
/// name and the rest is exception:
///
/// ```
/// # use terminal_choice::Choice;
/// let plain = Choice::named("zed");
/// let more = Choice::named("mullvad").ticked().heading("# vpn").tags(&["network"]);
/// assert!(!plain.checked && plain.enabled);
/// assert!(more.checked && more.heading.as_deref() == Some("# vpn"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What it is called, and what the answers give back.
    pub name: String,
    /// Ticked, for a checkbox group. A radio's selection lives in [`Item::Radio::chosen`] —
    /// exactly one option is picked, which a per-option flag could contradict.
    pub checked: bool,
    /// A sub-title drawn above this option — what a long list needs to read as sections without
    /// becoming several groups and several answers. Multi-line headings draw on several lines.
    pub heading: Option<String>,
    /// Whether this option is the user's to change. A disabled one is drawn dim and the cursor
    /// never lands on it, but it keeps its state and still counts: "already true, not yours".
    pub enabled: bool,
    /// What it IS, for filtering — see [`Form::filters`].
    pub tags: Vec<String>,
    /// Whether `checked` is owed to a suggestion rather than a fact — see [`Choice::suggest`].
    pub suggested: bool,
    /// Tags this option needs a COMPANION to carry — see [`Choice::requires`].
    pub requires: Vec<String>,
}

impl Choice {
    /// An option that is clear, live, unheaded and untagged — which most of them are.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            checked: false,
            heading: None,
            enabled: true,
            tags: Vec::new(),
            suggested: false,
            requires: Vec::new(),
        }
    }

    /// Arrives ticked, as a FACT about the machine. Clearing it is a deviation, and marked red.
    #[must_use]
    pub fn ticked(mut self) -> Self {
        self.checked = true;
        self
    }

    /// A sub-title above this option.
    #[must_use]
    pub fn heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    /// Shown and counted, but not the user's to change.
    #[must_use]
    pub fn locked(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// What this option is, for [`Form::filters`].
    #[must_use]
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
        self
    }

    /// Recommend it: tick it, and draw it blue to say the form put the tick there.
    ///
    /// An option that is ALREADY ticked comes back untouched — white, and still a fact. See
    /// [`GridCell::suggest`], which says the same for a grid and explains why.
    #[must_use]
    pub fn suggest(mut self) -> Self {
        if !self.checked {
            self.checked = true;
            self.suggested = true;
        }
        self
    }
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
    /// What this row IS, for filtering — see [`Form::filters`].
    ///
    /// Carried by the row rather than registered separately, because whoever builds a row
    /// already knows: `os_ricing` writes `package.tags` here in the same expression that writes
    /// the label. A second pass to attach them would be a second place to forget.
    pub tags: Vec<String>,
    /// Tags this row needs a COMPANION to carry — see [`Form::unmet`]. Empty for almost every
    /// row, which is why it is last.
    pub requires: Vec<String>,
}

impl GridRow {
    /// A row with a name, no cells and nothing else — everything past the label is exception.
    ///
    /// A builder, like [`Choice`] beside it and `software_inventory`'s `Package`, and for a
    /// reason this struct learned the hard way: adding `requires` meant editing thirteen literal
    /// constructions across two crates, twelve of which had nothing to say about it. The fields
    /// stay public, because reading them is most of what happens to a row.
    ///
    /// A row with NO cells draws as a line of gaps, which is the same thing a short row already
    /// does about the columns it does not reach. Nothing forbids it and nothing needs to.
    ///
    /// ```
    /// # use terminal_choice::{GridCell, GridRow};
    /// let row = GridRow::named("git")
    ///     .heading("# tools")
    ///     .note("version control")
    ///     .cells(vec![GridCell::set(None)])
    ///     .tags(&["vcs", "terminal"]);
    /// assert_eq!(row.heading.as_deref(), Some("# tools"));
    /// assert!(GridRow::named("bare").tags.is_empty());
    /// ```
    #[must_use]
    pub fn named(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            heading: None,
            note: None,
            cells: Vec::new(),
            tags: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// The sub-title drawn above this row, breaking a long table into sections.
    #[must_use]
    pub fn heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    /// The dim remark after the label — what the thing IS, where the name does not say.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The boxes, one per column.
    #[must_use]
    pub fn cells(mut self, cells: Vec<GridCell>) -> Self {
        self.cells = cells;
        self
    }

    /// What this row IS, for filtering — see [`Form::filters`].
    #[must_use]
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
        self
    }

    /// Tags this row needs a companion to carry — see [`Form::unmet`].
    #[must_use]
    pub fn requires(mut self, tags: &[&str]) -> Self {
        self.requires = tags.iter().map(|tag| (*tag).to_string()).collect();
        self
    }
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
    /// This tick is a SUGGESTION, not a fact about the machine — see [`GridCell::suggest`].
    pub suggested: bool,
}

impl GridCell {
    /// A cell the user may tick, doing `on_set`.
    #[must_use]
    pub fn open(on_set: Option<String>) -> Self {
        Self { checked: false, enabled: true, boxed: true, on_set, on_clear: None, suggested: false }
    }

    /// A cell that arrives ticked, and would do `on_clear` if emptied.
    #[must_use]
    pub fn set(on_clear: Option<String>) -> Self {
        Self { checked: true, enabled: true, boxed: true, on_set: None, on_clear, suggested: false }
    }

    /// Recommend this cell: tick it, and draw it blue to say the form put the tick there.
    ///
    /// Blue is exempt from the red mark. That exemption is the whole point: clearing a tick the
    /// form arrived with reads as undoing a fact, and is worth flagging — but a suggestion
    /// asserts nothing, so declining one is an ordinary answer, and marking it would tell the
    /// user they had broken something by disagreeing.
    ///
    /// A cell that is ALREADY ticked comes back untouched — white, and still a fact. A thing
    /// does not become a recommendation by being recommended: it would have been ticked
    /// regardless, so colouring it blue would credit the form with something that was already
    /// true, and clearing it is still a deviation.
    ///
    /// A suggestion counts in the answers exactly as any other tick. What it changes is what a
    /// deviation is measured against, not what is being asked.
    ///
    /// ```
    /// # use terminal_choice::GridCell;
    /// assert!(GridCell::open(None).suggest().suggested, "an empty box takes the hint");
    /// assert!(!GridCell::set(None).suggest().suggested, "a fact stays a fact");
    /// ```
    #[must_use]
    pub fn suggest(mut self) -> Self {
        if !self.checked {
            self.checked = true;
            self.suggested = true;
        }
        self
    }

    /// A box that is shown but not the user's to change — dark grey, and the cursor skips it.
    ///
    /// Still a BOX rather than a gap, because the two say different things: a box says this
    /// choice exists and something would have to change for it to be takeable, a gap says there
    /// is no choice here at all. It carries no preview lines: nothing can focus it, so nothing
    /// would ever show them.
    #[must_use]
    pub fn locked(checked: bool) -> Self {
        Self { checked, enabled: false, boxed: true, on_set: None, on_clear: None, suggested: false }
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
    /// Make the sub-titles interactive: each becomes a row the cursor can rest on, and folds the
    /// options beneath it out of sight.
    ///
    /// Off by default, and deliberately a whole-form choice rather than a per-heading one. A
    /// heading is a heading; whether a form is the KIND that folds is a fact about the form —
    /// four sections of three want reading at a glance, four hundred packages want folding — and
    /// a form with no headings is unaffected either way, since there is nothing to fold.
    pub collapsible: bool,
    /// Which sections are folded shut, as `(item, slot)` — the item they belong to, and the slot
    /// their heading stands above.
    ///
    /// Display state, not an answer. A folded section's boxes keep every tick they had and still
    /// appear in [`Form::answers_toml`]: folding hides a question, it does not withdraw it.
    ///
    /// Addressed by position rather than by heading text because two sections may legitimately be
    /// called the same thing. Meaningless while `collapsible` is false, and ignored then.
    pub collapsed: std::collections::BTreeSet<(usize, usize)>,
    /// The filter boxes, in display order — see [`Form::filters`]. Empty means no filter block,
    /// which is every form that never asks.
    pub filters: Vec<Rule>,
    /// The heading above that block.
    pub filter_label: String,
    /// Which filter boxes are currently CLEARED, by [`Rule::label`]. Display state, like
    /// `collapsed`: an entry filtered out of sight keeps every answer it had.
    pub excluded: std::collections::BTreeSet<String>,
    /// Entries this machine cannot offer at all — greyed out, not hidden, and not the user's to
    /// change. Each [`Rule::label`] says WHY, so a caller can explain itself.
    ///
    /// Deliberately not filter boxes, and the difference is who the answer belongs to. A filter
    /// is a PREFERENCE, and a preference is revisable: "I do not much like terminal programs, but
    /// this one looks worth a go" is a sentence somebody says, so the box has to let them say it.
    /// Nobody revisably runs one program on a display server they are not running. That is the
    /// machine's answer rather than the user's, so it greys the row instead of offering a box —
    /// and greyed rather than hidden, because a reader should see that the choice exists and why
    /// it is out of reach.
    pub incompatible: Vec<Rule>,
    /// At most ONE chosen entry may match each of these, form-wide. A second is a conflict, and
    /// the form refuses to submit while it stands — see [`Form::objections`].
    ///
    /// Said in tags rather than as pairs of names: "at most one `display-manager`" is one line
    /// however many display managers the catalogue grows, where a list of incompatible pairs is
    /// quadratic and needs editing every time one is added.
    pub exclusive: Vec<Rule>,
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
            options: options.iter().map(|name| Choice::named(*name)).collect(),
        });
        self
    }

    /// A choose-one group: nothing chosen, all selectable, no sub-titles.
    pub fn radio(mut self, label: impl Into<String>, options: &[&str]) -> Self {
        self.items.push(Item::Radio {
            label: label.into(),
            chosen: None,
            options: options.iter().map(|name| Choice::named(*name)).collect(),
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

    /// Offer a block of tick-boxes, one per tag, that hides entries carrying the tags it clears.
    ///
    /// `label` heads the block; each box is `(terms, wording)`, where the terms are a [`Rule`] in
    /// the `["terminal", "!gui"]` shorthand. Every box starts ticked, so a form opens showing
    /// everything.
    ///
    /// The list is the CALLER's: it decides what is worth filtering on. Entries carry their own
    /// tags already ([`GridRow::tags`]), so nothing has to be registered here twice.
    ///
    /// ## What clearing one does
    ///
    /// An entry is hidden when ANY CLEARED box governs it — that is, when its tags satisfy that
    /// box's rule. Not shown when some other box still does.
    ///
    /// Rules rather than plain tags because the interesting filters are negative. `terminal-only`
    /// is not the `terminal` tag: a thing carrying both `terminal` and `gui` is neither
    /// terminal-only nor GUI-only, and clearing either box must leave it alone. Written as tags
    /// it would vanish from both, which is the wrong answer twice.
    ///
    /// An entry no cleared box governs is never hidden — including an entry with no tags, which
    /// no rule with any positive term can match.
    ///
    /// ```
    /// # use terminal_choice::{Form, GridCell, GridRow};
    /// let row = |label: &str, tags: &[&str]| {
    ///     GridRow::named(label).cells(vec![GridCell::open(None)]).tags(tags)
    /// };
    /// let mut form = Form::new()
    ///     .grid("packages", &["apt"], vec![
    ///         row("ripgrep", &["terminal"]),
    ///         row("zoom", &["gui", "spyware"]),
    ///     ])
    ///     .filters("Include", &[
    ///         (&["terminal", "!gui"][..], "terminal-only"),
    ///         (&["gui", "!terminal"][..], "GUI-only"),
    ///         (&["spyware"][..],           "privacy-infringing"),
    ///     ]);
    ///
    /// assert!(!form.filtered_out(0, 1), "everything shows to begin with");
    ///
    /// // Clearing an exclusion box removes what it governs, other tags notwithstanding.
    /// form.excluded.insert("privacy-infringing".into());
    /// assert!(form.filtered_out(0, 1), "zoom goes, gui tag or not");
    /// assert!(!form.filtered_out(0, 0), "ripgrep is governed by neither");
    ///
    /// // And the negative half: zoom is gui AND terminal-less, so GUI-only governs it too…
    /// form.excluded.clear();
    /// form.excluded.insert("GUI-only".into());
    /// assert!(form.filtered_out(0, 1));
    /// // …while terminal-only does not touch it.
    /// form.excluded.clear();
    /// form.excluded.insert("terminal-only".into());
    /// assert!(!form.filtered_out(0, 1), "zoom is not terminal-only");
    /// assert!(form.filtered_out(0, 0), "ripgrep is");
    /// ```
    pub fn filters(mut self, label: impl Into<String>, boxes: &[(&[&str], &str)]) -> Self {
        self.filter_label = label.into();
        self.filters = boxes.iter().map(|(terms, said)| Rule::of(*said, terms)).collect();
        self
    }

    /// The tags carried by slot `slot` of item `index`.
    #[must_use]
    pub fn tags_at(&self, index: usize, slot: usize) -> &[String] {
        match self.items.get(index) {
            Some(Item::Checkboxes { options, .. } | Item::Radio { options, .. }) => {
                options.get(slot).map_or(&[][..], |o| o.tags.as_slice())
            }
            Some(Item::Grid { rows, .. }) => rows.get(slot).map_or(&[][..], |row| &row.tags),
            _ => &[],
        }
    }

    /// Every box of the filter block, in display order: the ones the caller offered, then one
    /// per incompatibility rule.
    ///
    /// An incompatibility gets a box FOR FREE, and that is the whole reason the two lists join
    /// here rather than being drawn separately. A rule that locks entries away has already
    /// decided they cannot be picked; all a box adds is the choice to stop looking at them. It
    /// hides nothing the user could have had — which is what makes it safe to offer, and
    /// different in kind from every other box in the block.
    ///
    /// It also answers the question the greying could not. A dim row with no explanation is a
    /// puzzle; a dim row plus a box saying `incompatible display server` is a sentence. The
    /// label does both jobs, which is why there is one string and not two.
    ///
    /// [`Rule::label`] is the identity in [`Form::excluded`], so a label repeated across the two
    /// lists would make one box work the other's rows. Keep them distinct.
    pub fn filter_boxes(&self) -> impl Iterator<Item = &Rule> {
        self.filters.iter().chain(self.incompatible.iter())
    }

    /// Whether slot `slot` of item `index` carries a tag the user has cleared.
    #[must_use]
    pub fn filtered_out(&self, index: usize, slot: usize) -> bool {
        if self.excluded.is_empty() {
            return false;
        }
        let tags = self.tags_at(index, slot);
        self.filter_boxes()
            .filter(|rule| self.excluded.contains(&rule.label))
            .any(|rule| rule.matches(tags))
    }

    /// Grey out every entry matching one of `rules` — see [`Form::incompatible`].
    ///
    /// Each pair is the rule's terms and the reason: `(&["wayland-only"], "this session is X11")`.
    /// Terms take the same `!` negation [`Rule::of`] reads everywhere else.
    pub fn incompatible(mut self, rules: &[(&[&str], &str)]) -> Self {
        self.incompatible = rules.iter().map(|(terms, why)| Rule::of(*why, terms)).collect();
        self
    }

    /// Allow at most one chosen entry per rule — see [`Form::exclusive`].
    pub fn exclusive(mut self, rules: &[(&[&str], &str)]) -> Self {
        self.exclusive = rules.iter().map(|(terms, said)| Rule::of(*said, terms)).collect();
        self
    }

    /// Whether slot `slot` of item `index` is one this machine cannot offer, and why.
    ///
    /// Evaluated on demand rather than baked into each entry's `enabled` at build time, so that
    /// the answer cannot depend on the order a form was assembled in — the same reason
    /// [`Form::filtered_out`] is a question and not a stored flag.
    #[must_use]
    pub fn incompatible_at(&self, index: usize, slot: usize) -> Option<&Rule> {
        if self.incompatible.is_empty() {
            return None;
        }
        let tags = self.tags_at(index, slot);
        self.incompatible.iter().find(|rule| rule.matches(tags))
    }

    /// Whether slot `slot` of item `index` is CHOSEN — ticked, picked, or, in a grid, ticked in
    /// any column at all.
    ///
    /// A grid row is chosen if any one of its cells is, because the columns are ways of having
    /// the same thing: a package installed through apt is installed.
    #[must_use]
    pub fn chosen_at(&self, index: usize, slot: usize) -> bool {
        match self.items.get(index) {
            Some(Item::Checkboxes { options, .. }) => {
                options.get(slot).is_some_and(|entry| entry.checked)
            }
            Some(Item::Radio { chosen, .. }) => *chosen == Some(slot),
            Some(Item::Grid { rows, .. }) => {
                rows.get(slot).is_some_and(|row| row.cells.iter().any(|cell| cell.checked))
            }
            _ => false,
        }
    }

    /// The tags slot `slot` of item `index` needs a companion to carry.
    #[must_use]
    pub fn requires_at(&self, index: usize, slot: usize) -> &[String] {
        match self.items.get(index) {
            Some(Item::Checkboxes { options, .. } | Item::Radio { options, .. }) => {
                options.get(slot).map_or(&[][..], |entry| entry.requires.as_slice())
            }
            Some(Item::Grid { rows, .. }) => {
                rows.get(slot).map_or(&[][..], |row| row.requires.as_slice())
            }
            _ => &[],
        }
    }

    /// Every tag some chosen entry demands and no chosen entry supplies, in the order first
    /// demanded and without repeats.
    ///
    /// "At least one of EACH", not one of the list: an entry requiring two tags needs a companion
    /// for both, or two requirements would collapse into one that either could answer.
    ///
    /// An entry may satisfy its own requirement, and that is not a loophole worth closing — a
    /// package tagged as the backend it needs really does supply it.
    #[must_use]
    pub fn unmet(&self) -> Vec<String> {
        let chosen: Vec<(usize, usize)> = (0..self.items.len())
            .flat_map(|index| (0..self.slots(index)).map(move |slot| (index, slot)))
            .filter(|(index, slot)| self.chosen_at(*index, *slot))
            .collect();
        let supplied = |want: &String| {
            chosen.iter().any(|(index, slot)| self.tags_at(*index, *slot).contains(want))
        };
        let mut missing: Vec<String> = Vec::new();
        for (index, slot) in &chosen {
            for want in self.requires_at(*index, *slot) {
                if !supplied(want) && !missing.contains(want) {
                    missing.push(want.clone());
                }
            }
        }
        missing
    }

    /// Whether slot `slot` of item `index` would answer something currently demanded — the rows
    /// a form turns green to say "pick one of these".
    #[must_use]
    pub fn wanted_at(&self, index: usize, slot: usize) -> bool {
        self.wanted_among(&self.unmet(), index, slot)
    }

    /// [`Form::wanted_at`], against a [`Form::unmet`] the caller computed once.
    ///
    /// The split exists for the repaint: `unmet` walks every slot, and asking it fresh per row
    /// would make drawing quadratic in the table's length — unnoticeable at sixty rows and a
    /// stutter at the six hundred the catalogue is heading for. One computation per frame, then
    /// this per row.
    #[must_use]
    pub fn wanted_among(&self, unmet: &[String], index: usize, slot: usize) -> bool {
        if unmet.is_empty() || self.chosen_at(index, slot) {
            return false; // already taken; the green is for the ones still to pick from
        }
        let tags = self.tags_at(index, slot);
        unmet.iter().any(|want| tags.contains(want))
    }

    /// The exclusivity rules more than one chosen entry matches — see [`Form::exclusive`].
    #[must_use]
    pub fn conflicts(&self) -> Vec<&Rule> {
        self.exclusive
            .iter()
            .filter(|rule| {
                let hits = (0..self.items.len())
                    .flat_map(|index| (0..self.slots(index)).map(move |slot| (index, slot)))
                    .filter(|(index, slot)| self.chosen_at(*index, *slot))
                    .filter(|(index, slot)| rule.matches(self.tags_at(*index, *slot)))
                    .count();
                hits > 1
            })
            .collect()
    }

    /// Every reason this form refuses to be submitted, as lines to show. Empty means it will go.
    ///
    /// The two sources meet HERE and nowhere else. A conflict is a fact about the catalogue —
    /// one rule over the whole form, at most one — and a requirement is a fact about an entry in
    /// it — one rule per entry, at least one. They are stated in different places for that
    /// reason, and they arrive at the same button, which is why the refusal is one function
    /// rather than two competing ones.
    #[must_use]
    pub fn objections(&self) -> Vec<String> {
        let conflicts = self.conflicts().into_iter().map(|rule| rule.label.clone());
        let unmet = self.unmet().into_iter().map(|want| format!("nothing chosen is `{want}`"));
        conflicts.chain(unmet).collect()
    }

    /// Make the sub-titles fold (see `collapsible`). Every section opens expanded.
    pub fn collapsible(mut self) -> Self {
        self.collapsible = true;
        self
    }

    /// Start with the section above `slot` of item `index` folded shut — for a form that opens
    /// with the long sections out of the way. No effect unless [`Form::collapsible`] is also set,
    /// and none if there is no heading at that slot.
    pub fn folded(mut self, index: usize, slot: usize) -> Self {
        self.collapsed.insert((index, slot));
        self
    }

    /// The heading standing above slot `slot` of item `index`, if there is one.
    ///
    /// One place that knows where headings live, so the two shapes that have them — the parallel
    /// `headings` of a choice group, and a grid row's own — are asked the same way.
    #[must_use]
    pub fn heading_at(&self, index: usize, slot: usize) -> Option<&str> {
        match self.items.get(index)? {
            Item::Checkboxes { options, .. } | Item::Radio { options, .. } => {
                options.get(slot)?.heading.as_deref()
            }
            Item::Grid { rows, .. } => rows.get(slot)?.heading.as_deref(),
            _ => None,
        }
    }

    /// How many slots item `index` has — options for a choice group, rows for a grid, none for
    /// anything else.
    pub(crate) fn slots(&self, index: usize) -> usize {
        match self.items.get(index) {
            Some(Item::Checkboxes { options, .. } | Item::Radio { options, .. }) => options.len(),
            Some(Item::Grid { rows, .. }) => rows.len(),
            _ => 0,
        }
    }

    /// The heading that governs slot `slot` — the nearest one at or above it.
    ///
    /// A section runs from its own heading to the next. Slots before the FIRST heading belong to
    /// no section at all and answer `None`: an item may open with a few loose options and only
    /// later break into parts, and those first few can never be folded away.
    pub(crate) fn section_head(&self, index: usize, slot: usize) -> Option<usize> {
        (0..=slot).rev().find(|above| self.heading_at(index, *above).is_some())
    }

    /// The last slot belonging to the section opened at `head` — where its `^` goes.
    pub(crate) fn section_tail(&self, index: usize, head: usize) -> usize {
        (head + 1..self.slots(index))
            .find(|slot| self.heading_at(index, *slot).is_some())
            .unwrap_or_else(|| self.slots(index))
            .saturating_sub(1)
    }

    /// Whether slot `slot` of item `index` is out of sight, folded away or filtered away.
    ///
    /// One predicate for both, because everything downstream — what draws, what the cursor can
    /// reach — cares only THAT a row is not shown. Two would be two chances to check one and
    /// forget the other.
    pub(crate) fn hidden(&self, index: usize, slot: usize) -> bool {
        let folded = self.collapsible
            && self
                .section_head(index, slot)
                .is_some_and(|head| self.collapsed.contains(&(index, head)));
        folded || self.filtered_out(index, slot)
    }

    /// Whether every slot of the section opened at `head` has been filtered away.
    ///
    /// A section emptied by a filter is not drawn at all. Leaving its `v`/`^` behind would fill
    /// the screen with headings over nothing, which is the opposite of what a filter is for —
    /// and folding an empty section is a control that does nothing.
    pub(crate) fn section_emptied(&self, index: usize, head: usize) -> bool {
        self.filter_boxes().next().is_some()
            && (head..=self.section_tail(index, head))
                .all(|slot| self.filtered_out(index, slot))
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
    /// let Item::Checkboxes { options, .. } = &form.items[0] else { panic!() };
    /// assert_eq!(options[0].checked, true, "the machine already has zed");
    /// assert_eq!(options[1].checked, false);
    /// assert_eq!(options[1].enabled, false, "and apt is not this user's to change");
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
                Item::Checkboxes { label: l, options, .. } if l == label => Some(
                    options.iter().filter(|o| o.checked).map(|o| o.name.as_str()).collect(),
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
            Item::Checkboxes { label: name, options: all, .. } if name == label => Some(
                all.iter()
                    .filter(|o| o.checked && options.contains(&o.name))
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
                chosen.and_then(|index| options.get(index)).map(|o| o.name.as_str())
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
                Item::Checkboxes { label, options, .. } => {
                    let picked: Vec<toml::Value> = options
                        .iter()
                        .filter(|o| o.checked)
                        .map(|o| toml::Value::String(o.name.clone()))
                        .collect();
                    table.insert(label.clone(), toml::Value::Array(picked));
                }
                Item::Radio { label, options, chosen, .. } => {
                    if let Some(option) = chosen.and_then(|index| options.get(index)) {
                        table.insert(label.clone(), toml::Value::String(option.name.clone()));
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
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!() };
        options[0].checked = true;
        options[2].checked = true;
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
            let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
            options.iter_mut().for_each(|option| option.checked = false);
            slots.iter().for_each(|slot| options[*slot].checked = true);
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
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
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
        let Item::Checkboxes { options, .. } = &mut form.items[0] else { panic!() };
        options[0].checked = true;
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
        let Item::Checkboxes { options, .. } = &mut form.items[1] else { panic!() };
        options[1].checked = true;
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

    // ——— requirements, conflicts, and what they do to Submit ———————————————

    /// A grid of packages, each row one tag and one requirement, ticked where `on` says.
    fn table(rows: &[(&str, &[&str], &[&str], bool)]) -> Form {
        Form::new().grid(
            "",
            &["apt"],
            rows.iter()
                .map(|(name, tags, requires, on)| {
                    GridRow::named(*name)
                        .cells(vec![if *on { GridCell::set(None) } else { GridCell::open(None) }])
                        .tags(tags)
                        .requires(requires)
                })
                .collect(),
        )
    }

    /// The dependency is said in TAGS, so any of several packages can answer it — which is the
    /// whole reason it is not said in names. Nothing is demanded until the dependent is chosen.
    #[test]
    fn a_requirement_names_a_kind_of_companion_not_a_particular_one() {
        let nothing_picked =
            table(&[("ncmpcpp", &[], &["audio-backend"], false), ("mpd", &["audio-backend"], &[], false)]);
        assert!(nothing_picked.unmet().is_empty(), "an unchosen entry demands nothing");

        let unmet = table(&[
            ("ncmpcpp", &[], &["audio-backend"], true),
            ("mpd", &["audio-backend"], &[], false),
            ("mpv", &[], &[], false),
        ]);
        assert_eq!(unmet.unmet(), ["audio-backend"]);
        assert!(unmet.wanted_at(0, 1), "mpd would answer it, so it goes green");
        assert!(!unmet.wanted_at(0, 2), "mpv would not");
        assert!(!unmet.wanted_at(0, 0), "nor does the entry that is asking");

        let met = table(&[
            ("ncmpcpp", &[], &["audio-backend"], true),
            ("mpd", &["audio-backend"], &[], true),
        ]);
        assert!(met.unmet().is_empty(), "a chosen supplier settles it");
        assert!(!met.wanted_at(0, 1), "and the green stops the moment it is picked");
    }

    /// "At least one of EACH", not one of the list. Two demands collapsing into one that either
    /// could answer is the mistake this pins down.
    #[test]
    fn two_requirements_need_two_companions() {
        let half = table(&[
            ("thing", &[], &["audio-backend", "session"], true),
            ("mpd", &["audio-backend"], &[], true),
            ("wayland-session", &["session"], &[], false),
        ]);
        assert_eq!(half.unmet(), ["session"], "one answered, one still open");

        // An entry may answer its own demand: a package tagged as the backend it needs really
        // does supply it, and calling that a loophole would forbid a true statement.
        let itself = table(&[("mpd", &["audio-backend"], &["audio-backend"], true)]);
        assert!(itself.unmet().is_empty());
    }

    /// Exclusivity is one rule over the WHOLE form, and it takes two chosen entries to break —
    /// which is the difference between "at most one" and "none".
    #[test]
    fn at_most_one_chosen_entry_may_match_an_exclusive_rule() {
        let one_of_each = |a: bool, b: bool| {
            let mut form = table(&[("gdm", &["display-manager"], &[], a), ("sddm", &["display-manager"], &[], b)]);
            form.exclusive = vec![Rule::of("only one display manager", &["display-manager"])];
            form
        };
        assert!(one_of_each(false, false).conflicts().is_empty(), "none is fine");
        assert!(one_of_each(true, false).conflicts().is_empty(), "one is the point");
        let both = one_of_each(true, true);
        assert_eq!(both.conflicts().len(), 1);
        assert_eq!(both.objections(), ["only one display manager"]);
    }

    /// The two sources meet at the button and only there — a fact about the catalogue and a fact
    /// about an entry in it, arriving as one list of reasons not to go.
    #[test]
    fn objections_gather_both_kinds_and_are_empty_when_the_form_is_sound() {
        let mut form = table(&[
            ("gdm", &["display-manager"], &[], true),
            ("sddm", &["display-manager"], &[], true),
            ("ncmpcpp", &[], &["audio-backend"], true),
        ]);
        form.exclusive = vec![Rule::of("only one display manager", &["display-manager"])];
        assert_eq!(
            form.objections(),
            ["only one display manager", "nothing chosen is `audio-backend`"]
        );

        let sound = table(&[("ncmpcpp", &[], &["audio-backend"], true), ("mpd", &["audio-backend"], &[], true)]);
        assert!(sound.objections().is_empty(), "nothing to say, so it may go");
    }

    /// A grid row is chosen if ANY of its columns is: the columns are ways of having the same
    /// thing, and a package installed through apt is installed.
    #[test]
    fn a_grid_row_counts_as_chosen_through_any_one_column() {
        let form = Form::new().grid(
            "",
            &["apt", "flatpak"],
            vec![GridRow::named("brave").cells(vec![GridCell::open(None), GridCell::set(None)]).tags(&["browser"])],
        );
        assert!(form.chosen_at(0, 0), "ticked in the second column, so it is had");
    }

    /// Greying is a claim about the MACHINE, so it is a rule the caller supplies rather than
    /// state on the entry — and it says why, which is what the label is for.
    #[test]
    fn an_incompatible_entry_is_named_by_its_tags_and_carries_its_reason() {
        let form = table(&[("hyprland", &["wayland-only"], &[], false), ("i3", &["x11-only"], &[], false)])
            .incompatible(&[(&["wayland-only"], "this session is X11")]);
        assert_eq!(form.incompatible_at(0, 0).map(|rule| rule.label.as_str()), Some("this session is X11"));
        assert_eq!(form.incompatible_at(0, 1), None, "i3 is the one that runs here");

        // No rules at all is the common case and must not cost a tag lookup per row.
        assert_eq!(table(&[("i3", &["x11-only"], &[], false)]).incompatible_at(0, 0), None);
    }
}
