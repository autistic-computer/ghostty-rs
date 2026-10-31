//! Port of `src/macos/foundation.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "foundation/array.rs"]
pub mod array;
#[path = "foundation/attributed_string.rs"]
pub mod attributed_string;
#[path = "foundation/base.rs"]
pub mod base;
#[path = "foundation/c.rs"]
pub mod c;
#[path = "foundation/character_set.rs"]
pub mod character_set;
#[path = "foundation/data.rs"]
pub mod data;
#[path = "foundation/dictionary.rs"]
pub mod dictionary;
#[path = "foundation/number.rs"]
pub mod number;
#[path = "foundation/string.rs"]
pub mod string;
#[path = "foundation/type.rs"]
pub mod r#type;
#[path = "foundation/url.rs"]
pub mod url;
// @generated-mods-end

pub use array::{Array, MutableArray};
pub use attributed_string::{AttributeKey, AttributedString, MutableAttributedString};
pub use base::{ComparisonResult, FourCharCode, Range};
pub use character_set::CharacterSet;
pub use data::Data;
pub use dictionary::{Dictionary, MutableDictionary};
pub use number::{Number, NumberType};
pub use r#type::{cf_release, cf_retain};
pub use string::{
    string_get_surrogate_pair_for_long_character, MutableString, String, StringComparison,
    StringEncoding,
};
pub use url::{URLPathStyle, URL};
