//! The two non-code ways a form starts: a TOML document, or CLI-style flags. Both land on the
//! same [`Form`], so everything downstream — the UI, the accessors, the answers — is identical
//! whichever door was used.

use crate::comments::{self, Comments};
use crate::{Choice, Condition, Form, Item, Warning};

/// How a definition's `check_if` / `enabled_if` predicates get answered — or `None` when nobody
/// offered to answer them. The strings are opaque here: what "path:zed" means is the caller's
/// business, the same way what a warning means is.
pub(crate) type Resolver<'a> = Option<&'a dyn Fn(&str) -> bool>;

/// One choice group's options, as a definition spells them.
#[derive(Default)]
struct Choices {
    options: Vec<String>,
    headings: Vec<Option<String>>,
    enabled: Vec<bool>,
    /// What each option's `check_if` resolved to.
    checked: Vec<bool>,
}

impl Choices {
    /// Zip the parallel arrays a definition is naturally read into, out into the per-option
    /// struct the form holds. Parsing produces columns; `Item` wants rows.
    fn rows(&self) -> Vec<Choice> {
        self.options
            .iter()
            .enumerate()
            .map(|(at, name)| Choice {
                name: name.clone(),
                checked: self.checked.get(at).copied().unwrap_or(false),
                heading: self.headings.get(at).cloned().flatten(),
                enabled: self.enabled.get(at).copied().unwrap_or(true),
                tags: Vec::new(),
                suggested: false,
            })
            .collect()
    }
}

/// TOML → [`Form`]. The schema is documented on [`Form::from_toml`]; errors name the item they
/// were found in, because "missing field `label`" without a position is a scavenger hunt.
pub(crate) fn from_toml(text: &str, resolve: Resolver) -> Result<Form, String> {
    // A second read of the same text, for the one thing the value parser drops: where the
    // comments were. See `comments` for why it is a parser and not a scan.
    let found = comments::harvest(text);
    let value: toml::Value = toml::from_str(text).map_err(|err| err.to_string())?;
    let table = value.as_table().ok_or("the top level must be a TOML table")?;
    let mut form = Form::new();
    if let Some(title) = table.get("title") {
        form.title =
            Some(title.as_str().ok_or("`title` must be a string")?.to_string());
    }
    for (key, field) in [("aligned", 0), ("mirror_duplicates", 1), ("scrub_colors", 2)] {
        if let Some(value) = table.get(key) {
            let on = value.as_bool().ok_or_else(|| format!("`{key}` must be a boolean"))?;
            match field {
                0 => form.aligned = on,
                1 => form.mirror_duplicates = on,
                _ => form.scrub_colors = on,
            }
        }
    }
    let items = table
        .get("item")
        .and_then(|v| v.as_array())
        .ok_or("no [[item]] entries — a form with nothing in it isn't one")?;
    for (index, entry) in items.iter().enumerate() {
        let at = |msg: &str| format!("item {}: {msg}", index + 1);
        let entry = entry.as_table().ok_or_else(|| at("must be a table"))?;
        let field = |name: &str| -> Result<&str, String> {
            entry
                .get(name)
                .and_then(|v| v.as_str())
                .ok_or_else(|| at(&format!("missing (or non-string) `{name}`")))
        };
        // A comment standing above this item in the file is a line above it on the screen.
        if let Some(said) = found.before_item(index) {
            form.items.push(Item::Comment(said.to_string()));
        }
        match field("kind")? {
            "comment" => form.items.push(Item::Comment(field("text")?.to_string())),
            "checkboxes" => {
                let label = field("label")?.to_string();
                let Choices { options, headings, enabled, mut checked } =
                    _choices(entry, index, &found, resolve, &at)?;
                for pre in entry.get("checked").and_then(|v| v.as_array()).into_iter().flatten() {
                    let name = pre.as_str().ok_or_else(|| at("`checked` must be strings"))?;
                    let slot = options
                        .iter()
                        .position(|option| option == name)
                        .ok_or_else(|| at(&format!("`checked` names {name:?}, not an option")))?;
                    checked[slot] = true;
                }
                let group = Choices { options, headings, enabled, checked };
                form.items.push(Item::Checkboxes { label, options: group.rows() });
            }
            "radio" => {
                let label = field("label")?.to_string();
                let Choices { options, headings, enabled, checked } =
                    _choices(entry, index, &found, resolve, &at)?;
                let chosen = match entry.get("chosen") {
                    // No stated choice: a `check_if` that came back true picks the option. The
                    // first one wins — a radio holds one answer, and the file said so twice.
                    None => checked.iter().position(|on| *on),
                    Some(value) => {
                        let name =
                            value.as_str().ok_or_else(|| at("`chosen` must be a string"))?;
                        Some(options.iter().position(|option| option == name).ok_or_else(
                            || at(&format!("`chosen` names {name:?}, not an option")),
                        )?)
                    }
                };
                let group = Choices { options, headings, enabled, checked };
                form.items.push(Item::Radio { label, chosen, options: group.rows() });
            }
            "text" => form.items.push(Item::Text {
                label: field("label")?.to_string(),
                value: entry
                    .get("value")
                    .map(|v| v.as_str().ok_or_else(|| at("`value` must be a string")))
                    .transpose()?
                    .unwrap_or_default()
                    .to_string(),
            }),
            other => {
                return Err(at(&format!(
                    "unknown kind {other:?} — the kinds are comment, checkboxes, radio, text"
                )))
            }
        }
    }
    _reject_duplicate_labels(&form)?;
    for (index, entry) in
        table.get("warning").and_then(|v| v.as_array()).into_iter().flatten().enumerate()
    {
        let at = |msg: &str| format!("warning {}: {msg}", index + 1);
        let entry = entry.as_table().ok_or_else(|| at("must be a table"))?;
        let text = entry
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| at("missing (or non-string) `text`"))?
            .to_string();
        let when = _condition(entry.get("when").ok_or_else(|| at("missing `when`"))?, &at)?;
        _reject_unknown_targets(&form, &when).map_err(|msg| at(&msg))?;
        form.warnings.push(Warning { text, when });
    }
    Ok(form)
}

/// One `when` table → a [`Condition`], recursing through the combinators. `at` is the caller's
/// position-stamping closure, so a mistake three levels down still says which warning it is in.
fn _condition(value: &toml::Value, at: &dyn Fn(&str) -> String) -> Result<Condition, String> {
    let table = value.as_table().ok_or_else(|| at("a condition must be a table"))?;
    let kind = table
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| at("a condition needs a string `kind`"))?;
    let string = |name: &str| -> Result<String, String> {
        table
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| at(&format!("`{kind}` needs a string `{name}`")))
    };
    let options = || -> Result<Vec<String>, String> {
        let listed: Vec<String> = table
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| at(&format!("`{kind}` needs an `options` array")))?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| at("`options` must all be strings"))?;
        match listed.is_empty() {
            true => Err(at("`options` may not be empty")),
            false => Ok(listed),
        }
    };
    let nested = || -> Result<Vec<Condition>, String> {
        table
            .get("of")
            .and_then(|v| v.as_array())
            .ok_or_else(|| at(&format!("`{kind}` needs an `of` array of conditions")))?
            .iter()
            .map(|one| _condition(one, at))
            .collect()
    };
    Ok(match kind {
        "checked" => Condition::Checked { label: string("label")?, options: options()? },
        "unchecked" => Condition::Unchecked { label: string("label")?, options: options()? },
        "checked_at_least" => Condition::CheckedAtLeast {
            label: string("label")?,
            options: options()?,
            count: table
                .get("count")
                .and_then(toml::Value::as_integer)
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| at("`checked_at_least` needs a non-negative integer `count`"))?,
        },
        "chosen" => Condition::Chosen { label: string("label")?, option: string("option")? },
        "all" => Condition::All(nested()?),
        "any" => Condition::Any(nested()?),
        "not" => Condition::Not(Box::new(_condition(
            table.get("of").ok_or_else(|| at("`not` needs an `of` condition"))?,
            at,
        )?)),
        other => {
            return Err(at(&format!(
                "unknown condition {other:?} — the kinds are checked, unchecked, \
                 checked_at_least, chosen, all, any, not"
            )))
        }
    })
}

/// A rule aimed at a label or option the form hasn't got can never fire, which makes it INVISIBLE
/// rather than wrong — the worst way for a caution to fail. Refused here, where the typo is, the
/// same way a stale `checked = […]` pre-selection is.
fn _reject_unknown_targets(form: &Form, condition: &Condition) -> Result<(), String> {
    let group = |label: &str, radio: bool| -> Result<&Vec<Choice>, String> {
        form.items
            .iter()
            .find_map(|item| match item {
                Item::Checkboxes { label: name, options, .. } if !radio && name == label => {
                    Some(options)
                }
                Item::Radio { label: name, options, .. } if radio && name == label => Some(options),
                _ => None,
            })
            .ok_or_else(|| {
                let kind = if radio { "radio" } else { "checkbox" };
                format!("no {kind} group is labelled {label:?}")
            })
    };
    let known = |options: &[Choice], wanted: &[String]| -> Result<(), String> {
        match wanted.iter().find(|want| !options.iter().any(|o| o.name == **want)) {
            Some(stray) => Err(format!("{stray:?} is not one of that group's options")),
            None => Ok(()),
        }
    };
    match condition {
        Condition::Checked { label, options }
        | Condition::Unchecked { label, options }
        | Condition::CheckedAtLeast { label, options, .. } => known(group(label, false)?, options),
        Condition::Chosen { label, option } => {
            known(group(label, true)?, std::slice::from_ref(option))
        }
        Condition::All(of) | Condition::Any(of) => {
            of.iter().try_for_each(|one| _reject_unknown_targets(form, one))
        }
        Condition::Not(one) => _reject_unknown_targets(form, one),
    }
}

/// One `options` array, read into everything a choice group needs: the texts, the sub-titles the
/// file's comments put above them, and the two per-option predicates already answered.
///
/// An option is a bare string, or a table when it has more to say:
///
/// ```toml
/// options = [
///     "plain",
///     { name = "zed", check_if = "path:zed", enabled_if = "path:curl" },
/// ]
/// ```
fn _choices(
    entry: &toml::value::Table,
    index: usize,
    found: &Comments,
    resolve: Resolver,
    at: &impl Fn(&str) -> String,
) -> Result<Choices, String> {
    let listed = entry
        .get("options")
        .and_then(|v| v.as_array())
        .ok_or_else(|| at("missing `options` array"))?;
    if listed.is_empty() {
        return Err(at("`options` may not be empty"));
    }
    let mut choices = Choices::default();
    for (slot, value) in listed.iter().enumerate() {
        let (name, check_if, enabled_if) = match value {
            toml::Value::String(name) => (name.clone(), None, None),
            toml::Value::Table(table) => {
                let name = table
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| at("an option table needs a string `name`"))?
                    .to_string();
                let spec = |field: &str| -> Result<Option<String>, String> {
                    table
                        .get(field)
                        .map(|v| {
                            v.as_str().map(str::to_string).ok_or_else(|| {
                                at(&format!("{name:?}: `{field}` must be a string"))
                            })
                        })
                        .transpose()
                };
                let (check_if, enabled_if) = (spec("check_if")?, spec("enabled_if")?);
                (name, check_if, enabled_if)
            }
            _ => return Err(at("an option is a string, or a table carrying `name`")),
        };
        choices.options.push(name);
        choices.headings.push(found.before_option(index, slot).map(str::to_string));
        // The two defaults differ on purpose. A `check_if` nobody answered leaves the box clear;
        // an `enabled_if` nobody answered leaves it usable. The other way round, loading a file
        // with `enabled_if` and no resolver would hand back a form frozen solid.
        choices.checked.push(check_if.is_some_and(|spec| resolve.is_some_and(|ask| ask(&spec))));
        choices
            .enabled
            .push(enabled_if.is_none_or(|spec| resolve.is_none_or(|ask| ask(&spec))));
    }
    Ok(choices)
}

/// CLI flags → [`Form`], in the order the flags appear (a form is read top to bottom, so the
/// definition order IS meaning — which is why this is hand-walked rather than fed to an
/// arg-parser that groups repeated flags).
pub(crate) fn from_args(args: impl Iterator<Item = String>) -> Result<Form, String> {
    let mut form = Form::new();
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        let mut value = |flag: &str| {
            args.next().ok_or_else(|| format!("{flag} needs a value after it"))
        };
        match flag.as_str() {
            "--title" => form.title = Some(value("--title")?),
            "--aligned" => form.aligned = true,
            "--mirror-duplicates" => form.mirror_duplicates = true,
            "--scrub-colors" => form.scrub_colors = true,
            "--comment" => form.items.push(Item::Comment(value("--comment")?)),
            "--checkbox" => {
                let (label, options, picked) = _choice_spec(&value("--checkbox")?)?;
                let mut checked = vec![false; options.len()];
                for name in picked {
                    let slot = options
                        .iter()
                        .position(|option| *option == name)
                        .ok_or_else(|| format!("--checkbox {label:?}: {name:?} is not an option"))?;
                    checked[slot] = true;
                }
                let (headings, enabled) = _plain(options.len());
                let group = Choices { options, headings, enabled, checked };
                form.items.push(Item::Checkboxes { label, options: group.rows() });
            }
            "--radio" => {
                let (label, options, picked) = _choice_spec(&value("--radio")?)?;
                let chosen = match picked.as_slice() {
                    [] => None,
                    [one] => Some(options.iter().position(|option| option == one).ok_or_else(
                        || format!("--radio {label:?}: {one:?} is not an option"),
                    )?),
                    _ => return Err(format!("--radio {label:?}: only one option can be chosen")),
                };
                let (headings, enabled) = _plain(options.len());
                // A radio's pre-selection is `chosen`; no per-option ticks come through a flag.
                let checked = vec![false; options.len()];
                let group = Choices { options, headings, enabled, checked };
                form.items.push(Item::Radio { label, chosen, options: group.rows() });
            }
            "--text" => {
                let spec = value("--text")?;
                let (label, prefill) = match spec.split_once('=') {
                    Some((label, prefill)) => (label.trim().to_string(), prefill.trim().to_string()),
                    None => (spec.trim().to_string(), String::new()),
                };
                form.items.push(Item::Text { label, value: prefill });
            }
            other => {
                return Err(format!(
                    "unknown flag {other:?} — the flags are --title, --comment, --checkbox, \
                     --radio, --text, --aligned, --mirror-duplicates, --scrub-colors"
                ))
            }
        }
    }
    if form.items.is_empty() {
        return Err("no items — a form with nothing in it isn't one".into());
    }
    _reject_duplicate_labels(&form)?;
    Ok(form)
}

/// What the flag door gives every group: no sub-titles, everything selectable. Flags describe a
/// quick ad-hoc form, and neither headings nor predicates fit on one.
fn _plain(count: usize) -> (Vec<Option<String>>, Vec<bool>) {
    (vec![None; count], vec![true; count])
}

/// `"Label: a, b, c = b, c"` → the label, the options, the pre-selected names (possibly none).
fn _choice_spec(spec: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    let (label, rest) = spec
        .split_once(':')
        .ok_or_else(|| format!("{spec:?}: expected \"Label: option, option, …\""))?;
    let (listed, picked) = match rest.split_once('=') {
        Some((listed, picked)) => (listed, _names(picked)),
        None => (rest, Vec::new()),
    };
    let options = _names(listed);
    if options.is_empty() {
        return Err(format!("{spec:?}: no options after the ':'"));
    }
    Ok((label.trim().to_string(), options, picked))
}

/// Comma-separated names, trimmed, empties dropped.
fn _names(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect()
}

/// Answers are keyed by label, so two fields sharing one would silently shadow each other in the
/// output — refused at definition time, where the fix is obvious.
fn _reject_duplicate_labels(form: &Form) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for item in &form.items {
        let label = match item {
            Item::Comment(_) => continue,
            Item::Checkboxes { label, .. }
            | Item::Radio { label, .. }
            | Item::Text { label, .. }
            | Item::Grid { label, .. } => label,
        };
        // An empty label is deliberate anonymity (an embedded group whose owner reads `items`
        // directly), so several may coexist — they never become answer keys.
        if !label.is_empty() && !seen.insert(label.clone()) {
            return Err(format!("two items share the label {label:?} — answers are keyed by label"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        title = "Order"
        [[item]]
        kind = "comment"
        text = "Read me, can't touch me."
        [[item]]
        kind = "checkboxes"
        label = "Toppings"
        options = ["olives", "onion", "feta"]
        checked = ["onion"]
        [[item]]
        kind = "radio"
        label = "Size"
        options = ["S", "M", "L"]
        chosen = "M"
        [[item]]
        kind = "text"
        label = "Name"
        value = "Ada"
    "#;

    #[test]
    fn the_documented_schema_loads_every_kind() {
        let form = Form::from_toml(GOOD).expect("the doc example must load");
        assert_eq!(form.title.as_deref(), Some("Order"));
        assert_eq!(form.items.len(), 4);
        assert_eq!(form.checked("Toppings"), ["onion"], "pre-selection lands");
        assert_eq!(form.chosen("Size"), Some("M"));
        assert_eq!(form.text_value("Name"), Some("Ada"));
    }

    /// The documented warning schema, both spellings of a condition tree (inline and nested
    /// tables), and the point of the whole thing: each file states its OWN rules and wording.
    #[test]
    fn a_toml_file_states_its_own_warning_rules() {
        let form = Form::from_toml(
            r#"
            [[item]]
            kind = "checkboxes"
            label = "packages"
            options = ["mullvad", "wireguard", "zed"]
            checked = ["mullvad"]
            [[item]]
            kind = "radio"
            label = "Shell"
            options = ["bash", "fish"]
            chosen = "fish"

            [[warning]]
            text = "More than one VPN client."
            when = { kind = "checked_at_least", label = "packages", options = ["mullvad", "wireguard"], count = 2 }

            [[warning]]
            text = "fish without an editor."
            [warning.when]
            kind = "all"
            of = [
                { kind = "chosen", label = "Shell", option = "fish" },
                { kind = "unchecked", label = "packages", options = ["zed"] },
            ]
        "#,
        )
        .expect("the documented schema must load");

        assert_eq!(form.warnings.len(), 2);
        // One VPN checked, so the first is quiet; fish is chosen and zed is not, so the second
        // speaks — which also shows the nested-table spelling parsing to the same thing.
        assert_eq!(form.active_warnings(), ["fish without an editor."]);

        let mut louder = form.clone();
        let Item::Checkboxes { options, .. } = &mut louder.items[0] else { panic!() };
        options[1].checked = true;
        assert_eq!(louder.active_warnings().len(), 2, "both hold once a second VPN is on");
    }

    /// A rule aimed at something the form hasn't got can never fire — invisible, not merely
    /// wrong — so it is refused at load, with the position of the warning that holds it.
    #[test]
    fn warning_mistakes_are_refused_where_the_typo_is() {
        let e = |warning: &str| {
            let text = format!(
                "[[item]]\nkind = \"checkboxes\"\nlabel = \"packages\"\noptions = [\"zed\"]\n\
                 [[item]]\nkind = \"radio\"\nlabel = \"Shell\"\noptions = [\"bash\"]\n{warning}"
            );
            Form::from_toml(&text).unwrap_err()
        };
        let when = |body: &str| format!("[[warning]]\ntext = \"t\"\nwhen = {{ {body} }}");

        assert!(e(&when("kind = \"checked\", label = \"nope\", options = [\"zed\"]"))
            .contains("no checkbox group is labelled \"nope\""));
        assert!(e(&when("kind = \"checked\", label = \"packages\", options = [\"vim\"]"))
            .contains("\"vim\" is not one of that group's options"));
        assert!(
            e(&when("kind = \"chosen\", label = \"packages\", option = \"zed\""))
                .contains("no radio group is labelled"),
            "a checkbox group is not a radio"
        );
        assert!(e(&when("kind = \"lever\"")).contains("unknown condition \"lever\""));
        assert!(e(&when("kind = \"checked_at_least\", label = \"packages\", options = [\"zed\"]"))
            .contains("non-negative integer `count`"));
        assert!(e(&when("kind = \"checked\", label = \"packages\", options = []"))
            .contains("may not be empty"));
        assert!(e("[[warning]]\ntext = \"t\"").contains("missing `when`"));
        assert!(e("[[warning]]\nwhen = { kind = \"all\", of = [] }").contains("`text`"));
        // Every one carries the position, and a nested mistake reports its warning too.
        assert!(e(&when("kind = \"lever\"")).contains("warning 1"), "errors carry a position");
        let nested = "[[warning]]\ntext = \"t\"\nwhen = { kind = \"not\", of = \
                      { kind = \"checked\", label = \"nope\", options = [\"zed\"] } }";
        assert!(e(nested).contains("warning 1"), "a nested mistake still says which warning");
    }

    /// The file's own comments become part of the form, where they stood: above an item, and
    /// above an option inside a list. That is what lets one long choice group read as sections
    /// while staying ONE group — and so one label, one answer.
    #[test]
    fn comments_become_sub_titles_where_they_stood() {
        let form = Form::from_toml(
            r#"
            # about the packages
            [[item]]
            kind = "checkboxes"
            label = "Packages"
            options = [
                # dev-tools
                "zed",
                "helix",
                # web browsers
                "firefox",
            ]
        "#,
        )
        .expect("loads");

        assert_eq!(form.items[0], Item::Comment("about the packages".into()), "{:?}", form.items[0]);
        let Item::Checkboxes { options, .. } = &form.items[1] else { panic!() };
        let names: Vec<&str> = options.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["zed", "helix", "firefox"], "still one group, three options");
        assert_eq!(options[0].heading.as_deref(), Some("dev-tools"));
        assert_eq!(options[1].heading, None, "a sub-title covers the option under it, not the run");
        assert_eq!(options[2].heading.as_deref(), Some("web browsers"));
        // The sections did NOT become separate groups: one label still answers for all three.
        assert_eq!(form.items.len(), 2, "a comment item and a group: {:?}", form.items);
    }

    /// `check_if` and `enabled_if` are opaque here. Whoever loads the form decides what they
    /// mean — and through the plain door nobody decides, which must leave the form usable.
    #[test]
    fn predicates_are_answered_by_the_caller_or_defaulted_safely() {
        let text = r#"
            [[item]]
            kind = "checkboxes"
            label = "Packages"
            options = [
                "plain",
                { name = "zed", check_if = "yes:zed" },
                { name = "helix", check_if = "no:helix" },
                { name = "apt", enabled_if = "no:apt" },
                { name = "curl", enabled_if = "yes:curl" },
            ]
        "#;
        let answer = |spec: &str| spec.starts_with("yes:");

        let asked = Form::from_toml_with(text, answer).expect("loads");
        let Item::Checkboxes { options, .. } = &asked.items[0] else { panic!() };
        let ticks: Vec<bool> = options.iter().map(|o| o.checked).collect();
        let live: Vec<bool> = options.iter().map(|o| o.enabled).collect();
        assert_eq!(ticks, [false, true, false, false, false], "only the true check_if ticks");
        assert_eq!(live, [true, true, true, false, true], "only the false enabled_if locks");

        // Nobody answering: nothing pre-ticked, everything still usable. The asymmetry is the
        // point — defaulting `enabled_if` the other way would hand back a frozen form.
        let unasked = Form::from_toml(text).expect("loads");
        let Item::Checkboxes { options, .. } = &unasked.items[0] else { panic!() };
        assert!(options.iter().all(|o| !o.checked), "no answer, no tick");
        assert!(options.iter().all(|o| o.enabled), "no answer, nothing frozen");
    }

    /// A `check_if` on a radio picks its option, and the `checked = […]` list still layers on top
    /// of the predicates rather than replacing them.
    #[test]
    fn predicates_and_the_checked_list_combine() {
        let form = Form::from_toml_with(
            r#"
            [[item]]
            kind = "checkboxes"
            label = "Packages"
            options = ["a", { name = "b", check_if = "yes" }]
            checked = ["a"]
            [[item]]
            kind = "radio"
            label = "Shell"
            options = ["bash", { name = "fish", check_if = "yes" }]
        "#,
            |spec| spec == "yes",
        )
        .expect("loads");

        assert_eq!(form.checked("Packages"), ["a", "b"], "the list and the predicate both count");
        assert_eq!(form.chosen("Shell"), Some("fish"), "a radio's check_if picks it");
    }

    #[test]
    fn option_tables_are_checked_like_everything_else() {
        let e = |options: &str| {
            let text = format!(
                "[[item]]\nkind = \"checkboxes\"\nlabel = \"x\"\noptions = {options}"
            );
            Form::from_toml(&text).unwrap_err()
        };
        assert!(e("[{ check_if = \"a\" }]").contains("needs a string `name`"));
        assert!(e("[{ name = \"a\", check_if = 7 }]").contains("`check_if` must be a string"));
        assert!(e("[{ name = \"a\", enabled_if = 7 }]").contains("\"a\""), "named against its option");
        assert!(e("[7]").contains("an option is a string, or a table carrying `name`"));
        assert!(e("[]").contains("may not be empty"));
    }

    #[test]
    fn definition_mistakes_are_named_with_their_position() {
        let e = |text: &str| Form::from_toml(text).unwrap_err();
        assert!(e("").contains("no [[item]]"));
        assert!(e("[[item]]\nkind = \"lever\"").contains("unknown kind \"lever\""));
        assert!(e("[[item]]\nkind = \"radio\"\nlabel = \"x\"\noptions = []").contains("may not be empty"));
        let stale = "[[item]]\nkind = \"radio\"\nlabel = \"x\"\noptions = [\"a\"]\nchosen = \"b\"";
        assert!(e(stale).contains("not an option"));
        let twice = "[[item]]\nkind = \"text\"\nlabel = \"x\"\n[[item]]\nkind = \"text\"\nlabel = \"x\"";
        assert!(e(twice).contains("share the label"));
        assert!(e("[[item]]\nkind = \"comment\"").contains("item 1"), "errors carry a position");
    }

    #[test]
    fn the_form_flags_load_from_both_doors() {
        let toml_form = Form::from_toml(
            "aligned = true\nmirror_duplicates = true\n[[item]]\nkind = \"text\"\nlabel = \"x\"",
        )
        .unwrap();
        assert!(toml_form.aligned && toml_form.mirror_duplicates);
        let args_form =
            Form::from_args(["--aligned", "--mirror-duplicates", "--text", "x"]).unwrap();
        assert!(args_form.aligned && args_form.mirror_duplicates);
        assert!(Form::from_toml("aligned = \"yes\"\n[[item]]\nkind = \"text\"\nlabel = \"x\"")
            .unwrap_err()
            .contains("boolean"));
    }

    /// Anonymous (empty-label) groups exist for embedding: several may coexist, and none of
    /// them becomes an answer key.
    #[test]
    fn anonymous_groups_may_repeat_and_stay_out_of_the_answers() {
        let mut form = Form::new()
            .checkboxes("", &["one"])
            .comment("between")
            .checkboxes("", &["two"])
            .text("Name", "Ada");
        assert_eq!(form.items.len(), 4, "two anonymous groups coexist");
        if let crate::Item::Checkboxes { options, .. } = &mut form.items[0] {
            options[0].checked = true;
        }
        let answers = form.answers_toml();
        assert!(answers.contains("Name"), "{answers}");
        assert!(!answers.contains("one"), "anonymous answers are the owner's to read: {answers}");
    }

    #[test]
    fn args_build_the_same_form_in_flag_order() {
        let form = Form::from_args([
            "--title", "Order",
            "--comment", "hello",
            "--checkbox", "Toppings: olives, onion, feta = onion, feta",
            "--radio", "Size: S, M, L = M",
            "--text", "Name = Ada",
        ])
        .expect("the documented grammar must parse");
        assert_eq!(form.checked("Toppings"), ["onion", "feta"]);
        assert_eq!(form.chosen("Size"), Some("M"));
        assert_eq!(form.text_value("Name"), Some("Ada"));
        assert!(matches!(form.items[0], Item::Comment(_)), "order is the flags' order");
    }

    #[test]
    fn arg_mistakes_are_refused_with_the_grammar() {
        let e = |args: &[&str]| Form::from_args(args.iter().copied()).unwrap_err();
        assert!(e(&["--checkbox", "no colon here"]).contains("expected"));
        assert!(e(&["--radio", "Size: S, M = S, M"]).contains("only one"));
        assert!(e(&["--radio", "Size: S, M = L"]).contains("not an option"));
        assert!(e(&["--text"]).contains("needs a value"));
        assert!(e(&["--frobnicate", "x"]).contains("unknown flag"));
        assert!(e(&[]).contains("no items"));
    }
}
