//! Port of `src/apprt/gtk/ext/actions.zig`.
//!
//! STATUS: ported
//!
//! Zig connects each action's `activate` signal with a C callback that gets
//! the owning object as an unowned `*T` user-data pointer. The port connects
//! a closure holding a weak reference to the owner (so the action map, which
//! the owner usually holds, does not keep it alive) and calls the callback
//! with the owner while it is alive.

use gio::prelude::*;
use gtk4::prelude::*;

/// Check that an action name is valid.
///
/// Reimplementation of `g_action_name_is_valid()` so that it can be
/// used at comptime.
///
/// See:
/// https://docs.gtk.org/gio/type_func.Action.name_is_valid.html
pub(crate) const fn g_action_name_is_valid(name: &str) -> bool {
    let name = name.as_bytes();
    if name.is_empty() {
        return false;
    }

    let mut i = 0;
    while i < name.len() {
        match name[i] {
            b'-' | b'.' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => {}
            _ => return false,
        }
        i += 1;
    }

    true
}

/// `Action(T).Callback`
pub type Callback<T> = fn(&gio::SimpleAction, Option<&glib::Variant>, &T);

/// Function to create a structure for describing an action.
#[derive(Debug, Clone)]
pub struct Action<T> {
    pub name: &'static str,
    pub callback: Callback<T>,
    pub parameter_type: Option<glib::VariantType>,
    pub state: Option<glib::Variant>,
}

impl<T> Action<T> {
    /// Function to initialize a new action so that we can comptime check
    /// the name.
    ///
    /// (The name check happens at runtime in Rust; use [`action_name!`] for
    /// a compile-time check.)
    pub fn init(
        name: &'static str,
        callback: Callback<T>,
        parameter_type: Option<&glib::VariantTy>,
    ) -> Self {
        assert!(g_action_name_is_valid(name));

        Action {
            name,
            callback,
            parameter_type: parameter_type.map(|t| t.to_owned()),
            state: None,
        }
    }

    /// Function to initialize a new stateful action so that we can comptime
    /// check the name.
    pub fn init_stateful(
        name: &'static str,
        callback: Callback<T>,
        parameter_type: Option<&glib::VariantTy>,
        state: glib::Variant,
    ) -> Self {
        assert!(g_action_name_is_valid(name));
        Action {
            name,
            callback,
            parameter_type: parameter_type.map(|t| t.to_owned()),
            state: Some(state),
        }
    }
}

/// Checks an action name at compile time and evaluates to it.
#[macro_export]
macro_rules! gtk_action_name {
    ($name:literal) => {{
        const _: () = assert!($crate::apprt::gtk::ext::actions::g_action_name_is_valid($name));
        $name
    }};
}

/// Add actions to a widget that implements gio.ActionMap.
pub fn add<T: IsA<gio::ActionMap>>(this: &T, actions: &[Action<T>]) {
    add_to_map(this, this.upcast_ref::<gio::ActionMap>(), actions);
}

/// Add actions to the given map.
pub fn add_to_map<T: ObjectType>(this: &T, map: &impl IsA<gio::ActionMap>, actions: &[Action<T>]) {
    for entry in actions {
        assert!(g_action_name_is_valid(entry.name));
        let action = match &entry.state {
            Some(state) => {
                gio::SimpleAction::new_stateful(entry.name, entry.parameter_type.as_deref(), state)
            }
            None => gio::SimpleAction::new(entry.name, entry.parameter_type.as_deref()),
        };
        let weak = this.downgrade();
        let callback = entry.callback;
        action.connect_activate(move |action, parameter| {
            if let Some(this) = weak.upgrade() {
                callback(action, parameter, &this);
            }
        });
        map.add_action(&action);
    }
}

/// Add actions to a widget that doesn't implement ActionGroup directly.
pub fn add_as_group<T: IsA<gtk4::Widget>>(
    this: &T,
    name: &str,
    actions: &[Action<T>],
) -> gio::SimpleActionGroup {
    assert!(g_action_name_is_valid(name));

    // Collect our actions into a group since we're just a plain widget that
    // doesn't implement ActionGroup directly.
    let group = gio::SimpleActionGroup::new();

    add_to_map(this, &group, actions);

    this.insert_action_group(name, Some(&group));

    group
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::{g_value_holds, test_gtk};

    #[test]
    fn g_action_name_is_valid_() {
        assert!(g_action_name_is_valid("ring-bell"));
        assert!(!g_action_name_is_valid("ring_bell"));
        assert!(!g_action_name_is_valid(""));
        assert_eq!(gtk_action_name!("ring-bell"), "ring-bell");
    }

    #[test]
    fn adding_actions_to_an_object() {
        // This test requires a connection to an active display environment.
        let expected: i32 = (getrandom::u32().unwrap() % (i32::MAX as u32)) as i32 + 1;
        test_gtk::run(move || {
            let ctx = glib::MainContext::default();

            fn callback(_: &gio::SimpleAction, variant: Option<&glib::Variant>, this: &gtk4::Box) {
                let Some(variant) = variant else { return };
                assert!(variant.is_type(glib::VariantTy::INT32));

                let value = variant.get::<i32>().unwrap().to_value();
                this.set_property_from_value("spacing", &value);
            }

            let bx = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

            {
                let actions = [Action::<gtk4::Box>::init(
                    "test",
                    callback,
                    Some(glib::VariantTy::INT32),
                )];

                let _ = add_as_group(&bx, "test", &actions);
            }

            let parameter = expected.to_variant();

            assert!(bx.activate_action("test.test", Some(&parameter)).is_ok());

            // Zig blocks here (`iteration(true)`); activation is synchronous, so
            // a non-blocking iteration avoids hanging on an idle context.
            ctx.iteration(false);

            let value = bx.property_value("spacing");

            assert!(g_value_holds(Some(&value), glib::Type::I32));

            let actual: i32 = value.get().unwrap();
            assert_eq!(expected, actual);

            while ctx.iteration(false) {}
        });
    }
}
