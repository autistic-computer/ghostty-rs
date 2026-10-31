//! Port of `src/cli/action.zig`.
//!
//! STATUS: ported

use std::fmt;

use super::args::{ArgIterator, CliEnum, ProcessArgs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectError {
    /// Multiple actions were detected. You can specify at most one
    /// action on the CLI otherwise the behavior desired is ambiguous.
    MultipleActions,

    /// An unknown action was specified.
    InvalidAction,
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DetectError::MultipleActions => f.write_str("error.MultipleActions"),
            DetectError::InvalidAction => f.write_str("error.InvalidAction"),
        }
    }
}

impl std::error::Error for DetectError {}

/// An action enum that can be detected from CLI args. The enum's Zig tag
/// names (see [`cli_enum!`](super::args::cli_enum)) are the action names.
///
/// The type can implement [`Action::detect_special_case`] (Zig: the
/// `detectSpecialCase` decl) to allow handling of special cases.
pub trait Action: CliEnum {
    fn detect_special_case(arg: &str) -> Option<SpecialCase<Self>> {
        let _ = arg;
        None
    }
}

/// Detect the action from CLI args. `args` are all the process args
/// (including argv0).
pub fn detect_args<E: Action>(args: ProcessArgs) -> Result<Option<E>, DetectError> {
    let mut iter = args;
    detect_iter::<E, _>(&mut iter)
}

/// Detect the action from any iterator. Each iterator value should yield
/// a CLI argument such as "--foo".
///
/// The type E must be an enum with the available actions. If the type E
/// implements `detect_special_case`, then it will be called for each
/// argument to allow handling of special cases.
pub fn detect_iter<E: Action, I: ArgIterator + ?Sized>(iter: &mut I) -> Result<Option<E>, DetectError> {
    let mut fallback: Option<E> = None;
    let mut pending: Option<E> = None;
    while let Some(arg) = iter.next() {
        // Allow handling of special cases.
        if let Some(special) = E::detect_special_case(&arg) {
            match special {
                SpecialCase::Action(a) => return Ok(Some(a)),
                SpecialCase::Fallback(a) => fallback = Some(a),
                SpecialCase::AbortIfNoAction => {
                    if pending.is_none() {
                        return Ok(None);
                    }
                }
            }
        }

        // Commands must start with "+"
        let Some(name) = arg.strip_prefix('+') else {
            continue;
        };
        if pending.is_some() {
            return Err(DetectError::MultipleActions);
        }
        pending = Some(E::from_tag(name).ok_or(DetectError::InvalidAction)?);
    }

    // If we have an action, we always return that action, even if we've
    // seen "--help" or "-h" because the action may have its own help text.
    if pending.is_some() {
        return Ok(pending);
    }

    // If we have no action but we have a fallback, then we return that.
    if let Some(a) = fallback {
        return Ok(Some(a));
    }

    Ok(None)
}

/// The action enum E can implement `detect_special_case` to
/// return this enum in order to perform various special case actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialCase<E> {
    /// Immediately return this action.
    Action(E),

    /// Return this action if no other action is found.
    Fallback(E),

    /// If there is no pending action (we haven't seen an action yet)
    /// then we should return no action. This is kind of weird but is
    /// a special case to allow "-e" in Ghostty.
    AbortIfNoAction,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{cli_enum, ArgIteratorGeneral};

    cli_enum! {
        enum Enum { Foo => "foo", Bar => "bar", Baz => "baz" }
    }
    impl Action for Enum {}

    #[test]
    fn detect_direct_match() {
        let mut iter = ArgIteratorGeneral::init("+foo");
        let result = detect_iter::<Enum, _>(&mut iter).unwrap();
        assert_eq!(Enum::Foo, result.unwrap());
        let _ = (Enum::Bar, Enum::Baz);
    }

    #[test]
    fn detect_invalid_match() {
        let mut iter = ArgIteratorGeneral::init("+invalid");
        assert_eq!(
            Err(DetectError::InvalidAction),
            detect_iter::<Enum, _>(&mut iter)
        );
    }

    #[test]
    fn detect_multiple_actions() {
        let mut iter = ArgIteratorGeneral::init("+foo +bar");
        assert_eq!(
            Err(DetectError::MultipleActions),
            detect_iter::<Enum, _>(&mut iter)
        );
    }

    #[test]
    fn detect_no_match() {
        let mut iter = ArgIteratorGeneral::init("--some-flag");
        let result = detect_iter::<Enum, _>(&mut iter).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn detect_args_includes_argv0() {
        let result = detect_args::<Enum>(ProcessArgs::new(vec![
            "ghostty".to_owned(),
            "+baz".to_owned(),
        ]))
        .unwrap();
        assert_eq!(Some(Enum::Baz), result);
    }

    mod special_action {
        use super::super::*;
        use crate::cli::args::{cli_enum, ArgIteratorGeneral};

        cli_enum! {
            enum Enum { Foo => "foo", Bar => "bar" }
        }
        impl Action for Enum {
            fn detect_special_case(arg: &str) -> Option<SpecialCase<Self>> {
                if arg == "--special" {
                    Some(SpecialCase::Action(Enum::Foo))
                } else {
                    None
                }
            }
        }

        #[test]
        fn detect_special_case_action() {
            {
                let mut iter = ArgIteratorGeneral::init("--special +bar");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Foo, result.unwrap());
            }

            {
                let mut iter = ArgIteratorGeneral::init("+bar --special");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Foo, result.unwrap());
            }

            {
                let mut iter = ArgIteratorGeneral::init("+bar");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Bar, result.unwrap());
            }
        }
    }

    mod special_fallback {
        use super::super::*;
        use crate::cli::args::{cli_enum, ArgIteratorGeneral};

        cli_enum! {
            enum Enum { Foo => "foo", Bar => "bar" }
        }
        impl Action for Enum {
            fn detect_special_case(arg: &str) -> Option<SpecialCase<Self>> {
                if arg == "--special" {
                    Some(SpecialCase::Fallback(Enum::Foo))
                } else {
                    None
                }
            }
        }

        #[test]
        fn detect_special_case_fallback() {
            {
                let mut iter = ArgIteratorGeneral::init("--special");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Foo, result.unwrap());
            }

            {
                let mut iter = ArgIteratorGeneral::init("+bar --special");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Bar, result.unwrap());
            }

            {
                let mut iter = ArgIteratorGeneral::init("--special +bar");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Bar, result.unwrap());
            }
        }
    }

    mod special_abort {
        use super::super::*;
        use crate::cli::args::{cli_enum, ArgIteratorGeneral};

        cli_enum! {
            enum Enum { Foo => "foo", Bar => "bar" }
        }
        impl Action for Enum {
            fn detect_special_case(arg: &str) -> Option<SpecialCase<Self>> {
                if arg == "-e" {
                    Some(SpecialCase::AbortIfNoAction)
                } else {
                    None
                }
            }
        }

        #[test]
        fn detect_special_case_abort_if_no_action() {
            {
                let mut iter = ArgIteratorGeneral::init("-e");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert!(result.is_none());
            }

            {
                let mut iter = ArgIteratorGeneral::init("+foo -e");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert_eq!(Enum::Foo, result.unwrap());
            }

            {
                let mut iter = ArgIteratorGeneral::init("-e +bar");
                let result = detect_iter::<Enum, _>(&mut iter).unwrap();
                assert!(result.is_none());
            }
        }
    }
}
