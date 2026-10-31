//! Port of `src/font/opentype.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "opentype/glyf.rs"]
pub mod glyf;
#[path = "opentype/head.rs"]
pub mod head;
#[path = "opentype/hhea.rs"]
pub mod hhea;
#[path = "opentype/os2.rs"]
pub mod os2;
#[path = "opentype/post.rs"]
pub mod post;
#[path = "opentype/sfnt.rs"]
pub mod sfnt;
#[path = "opentype/svg.rs"]
pub mod svg;
// @generated-mods-end

pub use self::glyf::Glyf;
pub use self::head::Head;
pub use self::hhea::Hhea;
pub use self::os2::Os2;
pub use self::post::Post;
pub use self::svg::Svg;
