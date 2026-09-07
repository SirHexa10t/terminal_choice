//! The one thing `toml::Value` throws away: where the comments were.
//!
//! A definition file is written to be read, and the lines a person puts in it to break a long list
//! into `# dev-tools`, `# web browsers`, `# security` are part of what they meant. A data model
//! cannot carry them — comments are not data — so the text is parsed a second time by a
//! format-preserving reader that keeps every byte of decoration, and only the comment positions
//! are taken from it.
//!
//! Why a second parse rather than a hand-rolled scan: getting this wrong is quiet. A scanner that
//! miscounts an entry puts a heading over the wrong option and nothing complains. `toml_edit`
//! answers "what preceded this value" by construction, and the two crates share `toml`'s own
//! parser, datetime and writer underneath — the cost of correctness here is two small crates.

/// Where each comment sat, by the position of the thing it precedes.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Comments {
    /// Text before `[[item]]` number N (0-based).
    pub items: Vec<(usize, String)>,
    /// Text before option `slot` of item `item` — a sub-title inside one choice group.
    pub options: Vec<(usize, usize, String)>,
}

impl Comments {
    /// What precedes item `index`, if anything.
    pub fn before_item(&self, index: usize) -> Option<&str> {
        self.items.iter().find(|(at, _)| *at == index).map(|(_, text)| text.as_str())
    }

    /// What precedes option `slot` of item `index`, if anything.
    pub fn before_option(&self, index: usize, slot: usize) -> Option<&str> {
        self.options
            .iter()
            .find(|(item, option, _)| *item == index && *option == slot)
            .map(|(_, _, text)| text.as_str())
    }
}

/// Harvest the comments from a definition. Never fails: this is decoration, and a document the
/// value parser is about to reject anyway must not first die here with a worse message.
pub(crate) fn harvest(text: &str) -> Comments {
    let mut found = Comments::default();
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return found;
    };
    let Some(items) = document.get("item").and_then(toml_edit::Item::as_array_of_tables) else {
        return found;
    };
    for (index, table) in items.iter().enumerate() {
        if let Some(text) = spoken(table.decor().prefix()) {
            found.items.push((index, text));
        }
        let options = table.get("options").and_then(toml_edit::Item::as_array);
        for (slot, value) in options.into_iter().flatten().enumerate() {
            if let Some(text) = spoken(value.decor().prefix()) {
                found.options.push((index, slot, text));
            }
        }
    }
    found
}

/// The comment lines out of one run of decoration — blank lines and indentation dropped, the `#`
/// and one following space stripped. Consecutive lines join into one heading, so a two-line
/// remark stays one remark; `None` when the run was only whitespace.
fn spoken(decor: Option<&toml_edit::RawString>) -> Option<String> {
    let raw = decor?.as_str()?;
    let text = raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix('#'))
        .map(|line| line.strip_prefix(' ').unwrap_or(line).trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        title = "Order"
        # about the first group
        [[item]]
        kind = "checkboxes"
        label = "Packages"
        options = [
            # dev-tools
            "zed",
            "helix",

            # web browsers
            #   (two lines, one remark)
            "firefox",
            { name = "mullvad", check_if = "path:mullvad" },
        ]

        [[item]]
        kind = "radio"
        label = "Shell"
        options = ["bash", "fish"]
    "#;

    #[test]
    fn comments_are_found_where_they_sit() {
        let found = harvest(SAMPLE);
        assert_eq!(found.before_item(0), Some("about the first group"));
        assert_eq!(found.before_item(1), None, "a group nothing was said about");

        assert_eq!(found.before_option(0, 0), Some("dev-tools"));
        assert_eq!(found.before_option(0, 1), None, "a comment covers one option, not the run");
        assert_eq!(
            found.before_option(0, 2),
            Some("web browsers\n  (two lines, one remark)"),
            "consecutive lines stay one heading"
        );
        // Positions count ENTRIES, not lines: the inline table is the fourth option even though
        // it is written differently from the three before it.
        assert_eq!(found.before_option(0, 3), None);
        assert_eq!(found.before_option(1, 0), None, "a single-line array has nowhere to hide one");
    }

    /// Several options on one line, and options split oddly across lines — the entry index has to
    /// follow the VALUES, which is exactly what a line-counting scanner would get wrong.
    #[test]
    fn entries_are_counted_not_lines() {
        let found = harvest(
            "[[item]]\noptions = [\"a\", \"b\",\n  # here\n  \"c\", \"d\"]\n",
        );
        assert_eq!(found.before_option(0, 2), Some("here"), "third entry, second line");
        assert_eq!(found.before_option(0, 1), None);
        assert_eq!(found.before_option(0, 3), None);
    }

    /// A `#` inside a string is data, not a comment — the case a hand-rolled scanner trips over.
    #[test]
    fn a_hash_inside_a_value_is_not_a_comment() {
        let found = harvest("[[item]]\nlabel = \"# not a comment\"\noptions = [\"#tag\", \"b\"]\n");
        assert_eq!(found.before_item(0), None);
        assert_eq!(found.before_option(0, 0), None);
        assert_eq!(found.before_option(0, 1), None);
    }

    /// Decoration is not data: a document that will fail to parse must fail with the value
    /// parser's message, not with a worse one from here.
    #[test]
    fn unparseable_input_yields_nothing_rather_than_an_error() {
        assert_eq!(harvest("this is not toml at all ["), Comments::default());
        assert_eq!(harvest(""), Comments::default());
        assert_eq!(harvest("# a comment and nothing else"), Comments::default());
    }
}
