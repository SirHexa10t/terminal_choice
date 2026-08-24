//! Interactive terminal forms: put a form on the terminal, hand back what the user chose.
//!
//! A form is a sequence of [`Item`]s — checkbox groups (choose many), radio groups (choose one),
//! free-text fields, and comments, which are display-only: the cursor skips them and nothing can
//! change them. Build one in code (the builder methods on [`Form`]), from CLI-style args
//! ([`Form::from_args`]), or from a TOML file ([`Form::from_toml`]); then [`run`] it, and read
//! the answers back through the accessors or as one TOML document ([`Form::answers_toml`]).
//!
//! The interactive part is deliberately thin: rendering and key handling are pure functions in
//! [`ui`], tested without any terminal, and [`run`] is the small loop that connects them to a
//! real one.

mod definition;
pub mod prompts;
pub mod ui;

pub use prompts::{prompt_Yn, prompt_yN, prompt_yn}; // capitals mirror the [Y/n]/[y/N] each prints
pub use ui::{run, Outcome};

/// One entry of a form, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Display-only text: rendered dim, never focusable, never in the answers.
    Comment(String),
    /// Choose any number of `options`; `checked` runs parallel to them.
    Checkboxes { label: String, options: Vec<String>, checked: Vec<bool> },
    /// Choose exactly one of `options` (or none, if the user never picks).
    Radio { label: String, options: Vec<String>, chosen: Option<usize> },
    /// Free text, editable in place.
    Text { label: String, value: String },
}

/// A form: an optional title and the items, in the order they show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub title: Option<String>,
    pub items: Vec<Item>,
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

    /// A choose-many group, all boxes initially clear.
    pub fn checkboxes(mut self, label: impl Into<String>, options: &[&str]) -> Self {
        self.items.push(Item::Checkboxes {
            label: label.into(),
            checked: vec![false; options.len()],
            options: options.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// A choose-one group, nothing initially chosen.
    pub fn radio(mut self, label: impl Into<String>, options: &[&str]) -> Self {
        self.items.push(Item::Radio {
            label: label.into(),
            options: options.iter().map(|s| s.to_string()).collect(),
            chosen: None,
        });
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
    /// ```
    pub fn from_toml(text: &str) -> Result<Self, String> {
        definition::from_toml(text)
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
                Item::Checkboxes { label: l, options, checked } if l == label => Some(
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

    /// The chosen option of the radio group called `label`, if one was picked.
    pub fn chosen(&self, label: &str) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            Item::Radio { label: l, options, chosen } if l == label => {
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
                Item::Checkboxes { label, options, checked } => {
                    let picked: Vec<toml::Value> = options
                        .iter()
                        .zip(checked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| toml::Value::String(option.clone()))
                        .collect();
                    table.insert(label.clone(), toml::Value::Array(picked));
                }
                Item::Radio { label, options, chosen } => {
                    if let Some(index) = chosen {
                        table.insert(label.clone(), toml::Value::String(options[*index].clone()));
                    }
                }
                Item::Text { label, value } => {
                    table.insert(label.clone(), toml::Value::String(value.clone()));
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
