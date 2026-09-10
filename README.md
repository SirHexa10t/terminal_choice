# terminal_choice

Interactive terminal forms: checkboxes, radio buttons, text fields, and fixed comments — filled
with arrow keys and Space, returned as answers. A library first; the bundled `terminal_choice` binary runs
the same forms standalone.

## The form, three ways

**In code** (the library route):

```rust
let mut form = terminal_choice::Form::new()
    .title("Pizza order")
    .comment("Comments are display-only: the cursor skips them.")
    .checkboxes("Toppings", &["olives", "onion", "feta"])
    .radio("Size", &["S", "M", "L"])
    .text("Name", "");
match terminal_choice::run(&mut form)? {
    terminal_choice::Outcome::Submitted => {
        let toppings = form.checked("Toppings");   // Vec<&str>
        let size = form.chosen("Size");            // Option<&str>
        let name = form.text_value("Name");        // Option<&str>
    }
    terminal_choice::Outcome::Cancelled => { /* esc — treat the form's state as noise */ }
}
```

**From TOML** (`Form::from_toml`, or `terminal_choice --file form.toml`):

```toml
title = "Pizza order"

[[item]]
kind = "comment"
text = "Display-only; may span lines."

[[item]]
kind = "checkboxes"
label = "Toppings"
options = ["olives", "onion", "feta"]
checked = ["onion"]          # optional pre-selection

[[item]]
kind = "radio"
label = "Size"
options = ["S", "M", "L"]
chosen = "M"                 # optional

[[item]]
kind = "text"
label = "Name"
value = "optional prefill"
```

**From flags** (`Form::from_args`, or the binary), in the order given:

```sh
terminal_choice --title "Pizza order" \
       --comment "Display-only." \
       --checkbox "Toppings: olives, onion, feta = onion" \
       --radio    "Size: S, M, L = M" \
       --text     "Name"
```

## The binary's contract

The form draws on **stderr**; the answers print to **stdout** as one TOML document (keys sorted),
so `terminal_choice … > answers.toml` composes:

```toml
Name = "Ada"
Size = "L"
Toppings = ["olives", "feta"]
```

Text fields and checkbox groups always appear (an empty array means "asked, none apply"); a radio
nobody picked is omitted. Exit codes: `0` submitted, `1` cancelled (nothing printed), `2` the
form, file, or terminal was unusable.

## Keys

`↑`/`↓` move over the interactive rows — comments are never visited. In a grid `←`/`→` move along
the row and `↑`/`↓` between rows, keeping the column. A section's title is a stop, `>` shut or `v`
open: `→`, `Space` or `Tab` open a shut one and the cursor lands on its first entry (or stays on
the title if nothing inside can be selected); `←` folds an open one. The `^` closing line is drawn
but skipped. `Space` toggles a checkbox or picks a radio; in a text field it types, like any other
character. `Ctrl+A` ticks every checkbox, or clears them all when they already are. `Ctrl+S`
submits from wherever the cursor is — and does nothing while an objection stands, exactly like
`Enter` on the dimmed button. `Tab` folds the section the cursor is in, landing on its `>`, on
forms that fold; it is not listed on ones that do not. `Enter` confirms on `[ Submit ]` and
otherwise hops to the next row, so fill-Enter-fill-Enter walks the form. `Esc` (or `Ctrl+C`)
cancels.

## What the boxes mean

`[█]` was already so when the form opened — a fact about the machine, drawn with the cursor's own
block so nobody mistakes it for a choice. `[x]` is a tick the user added. A `[x]` in blue is a
suggestion: the form recommends it and asserts nothing, so declining it is an ordinary answer. A
red `[ ]` is a fact the user cleared — a removal, meant. Ticking a fact back brings the block back.

## Design notes

Two dependencies: `console` (raw keys, styling, line clearing) and `toml`. Rendering and key
handling are pure functions (`ui::compose`, `ui::apply`), tested without a terminal; `run` is the
small loop that connects them to one. Duplicate labels are rejected at definition time — answers
are keyed by label, and two fields sharing one would silently shadow each other.
