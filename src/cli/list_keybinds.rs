//! Port of `src/cli/list_keybinds.zig`.
//!
//! STATUS: ported
//!
//! The pretty printer uses the [`tui::vaxis`](super::tui::vaxis) stand-in
//! for libvaxis. Zig's `std.SinglyLinkedList` of triggers is a `Vec`.

use std::cmp::Ordering;
use std::io::{self, BufWriter, IsTerminal, Write};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::tui::{self, vaxis};
use crate::config::{self as configpkg, Config, Keybinds};
use crate::input::binding::{self as Binding, TriggerKey};

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If `true`, print out the default keybinds instead of the ones configured
        /// in the config file.
        pub default: bool = false,

        /// If `true`, print out documentation about the action associated with the
        /// keybinds.
        pub docs: bool = false,

        /// If `true`, print without formatting even if printing to a tty
        pub plain: bool = false,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"list-keybinds"`).
pub const HELP: &str = include_str!("list_keybinds_help.txt");

/// The `list-keybinds` command is used to list all the available keybinds for
/// Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let config = if opts.default {
        Config::default()
    } else {
        Config::load()?
    };

    let stdout = io::stdout();
    if tui::CAN_PRETTY_PRINT && !opts.plain && stdout.is_terminal() {
        return pretty_print(&config.keybind);
    }

    let mut out = String::new();
    config
        .keybind
        .format_entry_docs(&mut configpkg::entry_formatter("keybind", &mut out), opts.docs)?;

    // Don't forget to flush!
    let mut writer = BufWriter::with_capacity(1024, stdout.lock());
    writer.write_all(out.as_bytes())?;
    writer.flush()?;
    Ok(0)
}

#[derive(Debug, Clone)]
struct ChordBinding {
    table_name: Option<String>,
    triggers: Vec<Binding::Trigger>,
    actions: Vec<Binding::Action>,
}

fn mods_count(triggers: &[Binding::Trigger]) -> usize {
    let mut count = 0;
    for trigger in triggers {
        if trigger.mods.super_ {
            count += 1;
        }
        if trigger.mods.ctrl {
            count += 1;
        }
        if trigger.mods.shift {
            count += 1;
        }
        if trigger.mods.alt {
            count += 1;
        }
    }
    count
}

fn trigger_key_int(key: TriggerKey) -> i64 {
    // We want catch_all to sort last.
    match key {
        TriggerKey::Physical(key) => key as i32 as i64,
        TriggerKey::Unicode(key) => key as i64,
        TriggerKey::CatchAll => i32::MAX as i64,
    }
}

impl ChordBinding {
    // Order keybinds based on various properties
    //    1. Default bindings before table bindings (tables grouped at end)
    //    2. Longest chord sequence
    //    3. Most active modifiers
    //    4. Alphabetically by active modifiers
    //    5. Trigger key order
    //    6. Within tables, sort by table name
    // These properties propagate through chorded keypresses
    //
    // Adapted from Binding.lessThan
    fn less_than(lhs: &ChordBinding, rhs: &ChordBinding) -> bool {
        let lhs_has_table = lhs.table_name.is_some();
        let rhs_has_table = rhs.table_name.is_some();

        if lhs_has_table != rhs_has_table {
            return !lhs_has_table;
        }

        if lhs_has_table {
            let table_cmp = lhs
                .table_name
                .as_deref()
                .unwrap()
                .as_bytes()
                .cmp(rhs.table_name.as_deref().unwrap().as_bytes());
            if table_cmp != Ordering::Equal {
                return table_cmp == Ordering::Less;
            }
        }

        let lhs_len = lhs.triggers.len();
        let rhs_len = rhs.triggers.len();

        debug_assert!(lhs_len != 0);
        debug_assert!(rhs_len != 0);

        if lhs_len != rhs_len {
            return lhs_len > rhs_len;
        }

        let lhs_count = mods_count(&lhs.triggers);
        let rhs_count = mods_count(&rhs.triggers);

        if lhs_count != rhs_count {
            return lhs_count > rhs_count;
        }

        for (l, r) in lhs.triggers.iter().zip(rhs.triggers.iter()) {
            let l_int = l.mods.int();
            let r_int = r.mods.int();
            if l_int != r_int {
                return l_int > r_int;
            }
        }

        let mut i = 0;
        while i < lhs.triggers.len() && i < rhs.triggers.len() {
            let lhs_key = trigger_key_int(lhs.triggers[i].key);
            let rhs_key = trigger_key_int(rhs.triggers[i].key);

            i += 1;

            if i >= lhs.triggers.len() || i >= rhs.triggers.len() {
                return lhs_key < rhs_key;
            }

            if lhs_key != rhs_key {
                return lhs_key < rhs_key;
            }
        }

        // The previous loop will always return something on its final iteration so we cannot
        // reach this point
        unreachable!()
    }

    fn order(lhs: &ChordBinding, rhs: &ChordBinding) -> Ordering {
        if ChordBinding::less_than(lhs, rhs) {
            Ordering::Less
        } else if ChordBinding::less_than(rhs, lhs) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

/// Zig `{t}` / `{u}` of a trigger key.
fn trigger_key_string(key: TriggerKey) -> String {
    match key {
        TriggerKey::Physical(k) => k.as_str().to_owned(),
        TriggerKey::Unicode(c) => char::from_u32(c).map(String::from).unwrap_or_default(),
        TriggerKey::CatchAll => "catch_all".to_owned(),
    }
}

fn pretty_print(keybinds: &Keybinds) -> RunResult {
    // Set up vaxis
    let mut tty = vaxis::Tty::init()?;
    let mut vx = vaxis::Vaxis::init();

    // We know we are ghostty, so let's enable mode 2027. Vaxis normally does this but you need an
    // event loop to auto-enable it.
    vx.caps.unicode = true;
    tty.writer().write_all(vaxis::ctlseqs::UNICODE_SET.as_bytes())?;

    let result = pretty_print_inner(&mut tty, &mut vx, keybinds);

    let _ = tty.writer().write_all(vaxis::ctlseqs::UNICODE_RESET.as_bytes());
    vx.deinit(tty.writer());
    result
}

fn pretty_print_inner(tty: &mut vaxis::Tty, vx: &mut vaxis::Vaxis, keybinds: &Keybinds) -> RunResult {
    let winsize = tty.get_winsize()?;
    vx.resize(tty.writer(), winsize)?;

    // Collect default bindings, recursively flattening chords
    let (default_bindings, mut widest_chord) = iterate_bindings(&keybinds.set);

    let mut bindings: Vec<ChordBinding> = default_bindings;

    // Collect key table bindings
    let mut widest_table_prefix: u16 = 0;
    for (table_name, table) in &keybinds.tables {
        let (mut table_bindings, table_width) = iterate_bindings(table);
        for b in &mut table_bindings {
            b.table_name = Some(table_name.clone());
        }

        bindings.extend(table_bindings);
        widest_chord = widest_chord.max(table_width);
        widest_table_prefix = widest_table_prefix.max(vaxis::gwidth(table_name) + vaxis::gwidth("/"));
    }

    bindings.sort_by(ChordBinding::order);

    // Set up styles for each modifier
    let fg = |index: u8| vaxis::Style {
        fg: vaxis::Color::Index(index),
        ..Default::default()
    };
    let super_style = fg(1);
    let ctrl_style = fg(2);
    let alt_style = fg(3);
    let shift_style = fg(4);
    let table_style = fg(8);

    let seg = |text: &str, style: vaxis::Style| -> (String, vaxis::Style) { (text.to_owned(), style) };

    // Print the list
    for bind in &bindings {
        {
            let win = vx.window();
            win.clear();

            let print = |text: &str, style: vaxis::Style, col: u16| {
                win.print_segment(
                    vaxis::Segment {
                        text,
                        style,
                        ..Default::default()
                    },
                    vaxis::PrintOptions {
                        col_offset: col,
                        ..Default::default()
                    },
                )
            };

            let mut result = vaxis::PrintResult::default();

            if let Some(name) = &bind.table_name {
                result = print(name, table_style, result.col);
                result = print("/", table_style, result.col);
            }

            let none = vaxis::Style::default();
            for (i, trigger) in bind.triggers.iter().enumerate() {
                if trigger.mods.super_ {
                    result = print("super", super_style, result.col);
                    result = print(" + ", none, result.col);
                }
                if trigger.mods.ctrl {
                    result = print("ctrl ", ctrl_style, result.col);
                    result = print(" + ", none, result.col);
                }
                if trigger.mods.alt {
                    result = print("alt  ", alt_style, result.col);
                    result = print(" + ", none, result.col);
                }
                if trigger.mods.shift {
                    result = print("shift", shift_style, result.col);
                    result = print(" + ", none, result.col);
                }
                let key = trigger_key_string(trigger.key);
                result = print(&key, none, result.col);

                // Print a separator between chorded keys
                if i + 1 < bind.triggers.len() {
                    result = print(
                        "  >  ",
                        vaxis::Style {
                            bold: true,
                            fg: vaxis::Color::Index(6),
                            ..Default::default()
                        },
                        result.col,
                    );
                }
            }

            let mut action_col: u16 = widest_table_prefix + widest_chord + 3;
            for (i, act) in bind.actions.iter().enumerate() {
                if i > 0 {
                    let chain_result = print(
                        ", ",
                        vaxis::Style {
                            dim: true,
                            ..Default::default()
                        },
                        action_col,
                    );
                    action_col = chain_result.col;
                }

                let action = act.to_string();
                // If our action has an argument, we print the argument in a different color
                if let Some(idx) = action.find(':') {
                    let parts = [
                        seg(&action[..idx], none),
                        seg(
                            &action[idx..idx + 1],
                            vaxis::Style {
                                dim: true,
                                ..Default::default()
                            },
                        ),
                        seg(&action[idx + 1..], fg(5)),
                    ];
                    let segments: Vec<vaxis::Segment<'_>> = parts
                        .iter()
                        .map(|(text, style)| vaxis::Segment {
                            text,
                            style: *style,
                            ..Default::default()
                        })
                        .collect();
                    let print_result = win.print(
                        &segments,
                        vaxis::PrintOptions {
                            col_offset: action_col,
                            ..Default::default()
                        },
                    );
                    action_col = print_result.col;
                } else {
                    let print_result = print(&action, none, action_col);
                    action_col = print_result.col;
                }
            }
            let _ = result;
        }
        vx.pretty_print(tty.writer())?;
    }
    tty.writer().flush()?;
    Ok(0)
}

fn iterate_bindings(set: &Binding::Set) -> (Vec<ChordBinding>, u16) {
    let mut widest_chord: u16 = 0;
    let mut bindings: Vec<ChordBinding> = Vec::new();
    for (key, value) in set.bindings.iter() {
        let t: Binding::Trigger = **key;
        let width = {
            let mut buf = String::new();

            if t.mods.super_ {
                buf.push_str("super + ");
            }
            if t.mods.ctrl {
                buf.push_str("ctrl  + ");
            }
            if t.mods.alt {
                buf.push_str("alt   + ");
            }
            if t.mods.shift {
                buf.push_str("shift + ");
            }
            buf.push_str(&trigger_key_string(t.key));

            vaxis::gwidth(&buf)
        };

        match value {
            Binding::Value::Leader(leader) => {
                // Recursively iterate on the set of bindings for this leader key
                let (mut sub_bindings, max_width) = iterate_bindings(leader);

                // Prepend the current keybind onto the list of sub-binds
                for nb in &mut sub_bindings {
                    nb.triggers.insert(0, t);
                }

                // Add the longest sub-bind width to the current bind width along with a padding
                // of 5 for the '  >  ' spacer
                widest_chord = widest_chord.max(width + max_width + 5);
                bindings.extend(sub_bindings);
            }
            Binding::Value::Leaf(leaf) => {
                widest_chord = widest_chord.max(width);
                bindings.push(ChordBinding {
                    table_name: None,
                    triggers: vec![t],
                    actions: vec![leaf.action.clone()],
                });
            }
            Binding::Value::LeafChained(leaf) => {
                widest_chord = widest_chord.max(width);
                bindings.push(ChordBinding {
                    table_name: None,
                    triggers: vec![t],
                    actions: leaf.actions.clone(),
                });
            }
        }
    }

    (bindings, widest_chord)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keybinds_flatten_and_sort() {
        let config = Config::default();
        let (mut bindings, widest) = iterate_bindings(&config.keybind.set);
        assert!(!bindings.is_empty());
        assert!(widest > 0);
        bindings.sort_by(ChordBinding::order);
        // Longest chords and most modifiers sort first.
        for w in bindings.windows(2) {
            assert!(w[0].triggers.len() >= w[1].triggers.len());
            assert_ne!(Ordering::Greater, ChordBinding::order(&w[0], &w[1]));
        }

        // Table bindings sort after default bindings.
        let mut a = bindings[0].clone();
        a.table_name = Some("t".to_owned());
        assert!(ChordBinding::less_than(&bindings[0], &a));
        assert!(!ChordBinding::less_than(&a, &bindings[0]));
    }
}
