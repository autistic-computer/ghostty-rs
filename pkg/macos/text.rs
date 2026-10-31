//! Port of `src/macos/text.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "text/c.rs"]
pub mod c;
#[path = "text/font.rs"]
pub mod font;
#[path = "text/font_collection.rs"]
pub mod font_collection;
#[path = "text/font_descriptor.rs"]
pub mod font_descriptor;
#[path = "text/font_manager.rs"]
pub mod font_manager;
#[path = "text/frame.rs"]
pub mod frame;
#[path = "text/framesetter.rs"]
pub mod framesetter;
#[path = "text/line.rs"]
pub mod line;
#[path = "text/paragraph_style.rs"]
pub mod paragraph_style;
#[path = "text/run.rs"]
pub mod run;
#[path = "text/stylized_strings.rs"]
pub mod stylized_strings;
#[path = "text/typesetter.rs"]
pub mod typesetter;
// @generated-mods-end

pub use font::{Font, FontOrientation, FontTableTag};
pub use font_collection::FontCollection;
pub use font_descriptor::{
    attr, FontAttribute, FontAttributeType, FontDescriptor, FontSymbolicTraits, FontTraitKey,
    FontVariationAxisKey,
};
pub use font_manager::{
    create_font_descriptor_from_data, create_font_descriptors_from_data,
    create_font_descriptors_from_url,
};
pub use frame::Frame;
pub use framesetter::Framesetter;
pub use line::{Line, LineBoundsOptions};
pub use paragraph_style::{
    ParagraphStyle, ParagraphStyleSetting, ParagraphStyleSpecifier, WritingDirection,
};
pub use run::{Run, Status as RunStatus};
pub use stylized_strings::StringAttribute;
pub use typesetter::Typesetter;
