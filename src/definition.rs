//! The two non-code ways a form starts: a TOML document, or CLI-style flags. Both land on the
//! same [`Form`], so everything downstream — the UI, the accessors, the answers — is identical
//! whichever door was used.

use crate::{Form, Item};

/// TOML → [`Form`]. The schema is documented on [`Form::from_toml`]; errors name the item they
/// were found in, because "missing field `label`" without a position is a scavenger hunt.
pub(crate) fn from_toml(text: &str) -> Result<Form, String> {
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
        let options = || -> Result<Vec<String>, String> {
            let listed = entry
                .get("options")
                .and_then(|v| v.as_array())
                .ok_or_else(|| at("missing `options` array"))?
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| at("`options` must all be strings"))?;
            if listed.is_empty() {
                return Err(at("`options` may not be empty"));
            }
            Ok(listed)
        };
        match field("kind")? {
            "comment" => form.items.push(Item::Comment(field("text")?.to_string())),
            "checkboxes" => {
                let label = field("label")?.to_string();
                let options = options()?;
                let mut checked = vec![false; options.len()];
                for pre in entry.get("checked").and_then(|v| v.as_array()).into_iter().flatten() {
                    let name = pre.as_str().ok_or_else(|| at("`checked` must be strings"))?;
                    let slot = options
                        .iter()
                        .position(|option| option == name)
                        .ok_or_else(|| at(&format!("`checked` names {name:?}, not an option")))?;
                    checked[slot] = true;
                }
                form.items.push(Item::Checkboxes { label, options, checked });
            }
            "radio" => {
                let label = field("label")?.to_string();
                let options = options()?;
                let chosen = match entry.get("chosen") {
                    None => None,
                    Some(value) => {
                        let name =
                            value.as_str().ok_or_else(|| at("`chosen` must be a string"))?;
                        Some(options.iter().position(|option| option == name).ok_or_else(
                            || at(&format!("`chosen` names {name:?}, not an option")),
                        )?)
                    }
                };
                form.items.push(Item::Radio { label, options, chosen });
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
    Ok(form)
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
                form.items.push(Item::Checkboxes { label, options, checked });
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
                form.items.push(Item::Radio { label, options, chosen });
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
            Item::Checkboxes { label, .. } | Item::Radio { label, .. } | Item::Text { label, .. } => label,
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
        if let crate::Item::Checkboxes { checked, .. } = &mut form.items[0] {
            checked[0] = true;
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
