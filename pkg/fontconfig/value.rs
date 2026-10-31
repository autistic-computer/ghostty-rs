//! Port of `pkg/fontconfig/value.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::os::raw::{c_int, c_void};

use super::c;
use super::char_set::CharSet;
use super::lang_set::LangSet;
use super::matrix::Matrix;
use super::range::Range;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Unknown = c::FcTypeUnknown,
    Void = c::FcTypeVoid,
    Integer = c::FcTypeInteger,
    Double = c::FcTypeDouble,
    String = c::FcTypeString,
    Bool = c::FcTypeBool,
    Matrix = c::FcTypeMatrix,
    CharSet = c::FcTypeCharSet,
    FtFace = c::FcTypeFTFace,
    LangSet = c::FcTypeLangSet,
    Range = c::FcTypeRange,
}

impl Type {
    /// `@enumFromInt` for a raw `FcType`.
    pub fn from_raw(v: c::FcType) -> Type {
        match v {
            c::FcTypeVoid => Type::Void,
            c::FcTypeInteger => Type::Integer,
            c::FcTypeDouble => Type::Double,
            c::FcTypeString => Type::String,
            c::FcTypeBool => Type::Bool,
            c::FcTypeMatrix => Type::Matrix,
            c::FcTypeCharSet => Type::CharSet,
            c::FcTypeFTFace => Type::FtFace,
            c::FcTypeLangSet => Type::LangSet,
            c::FcTypeRange => Type::Range,
            _ => Type::Unknown,
        }
    }
}

/// Zig: `union(Type)`. Borrowed data (strings, matrices) is owned by the
/// pattern the value was read from; `'a` ties it to that pattern handle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    Unknown,
    Void,
    Integer(i32),
    Double(f64),
    String(&'a CStr),
    Bool(bool),
    Matrix(&'a Matrix),
    CharSet(CharSet),
    FtFace(*mut c_void),
    LangSet(LangSet),
    Range(Range),
}

impl<'a> Value<'a> {
    /// The active tag (Zig: `std.meta.activeTag`).
    pub fn tag(&self) -> Type {
        match self {
            Value::Unknown => Type::Unknown,
            Value::Void => Type::Void,
            Value::Integer(_) => Type::Integer,
            Value::Double(_) => Type::Double,
            Value::String(_) => Type::String,
            Value::Bool(_) => Type::Bool,
            Value::Matrix(_) => Type::Matrix,
            Value::CharSet(_) => Type::CharSet,
            Value::FtFace(_) => Type::FtFace,
            Value::LangSet(_) => Type::LangSet,
            Value::Range(_) => Type::Range,
        }
    }

    /// # Safety
    /// `cvalue` must be a valid `FcValue` whose pointers stay valid for `'a`.
    pub unsafe fn init(cvalue: &c::_FcValue) -> Value<'a> {
        match Type::from_raw(cvalue.type_) {
            Type::Unknown => Value::Unknown,
            Type::Void => Value::Void,
            Type::String => Value::String(CStr::from_ptr(cvalue.u.s as *const _)),
            Type::Integer => Value::Integer(cvalue.u.i),
            Type::Double => Value::Double(cvalue.u.d),
            Type::Bool => Value::Bool(cvalue.u.b == c::FcTrue as c::FcBool),
            Type::Matrix => Value::Matrix(&*(cvalue.u.m as *const Matrix)),
            Type::CharSet => Value::CharSet(CharSet::from_raw(cvalue.u.c as *mut _)),
            Type::FtFace => Value::FtFace(cvalue.u.f),
            Type::LangSet => Value::LangSet(LangSet::from_raw(cvalue.u.l as *mut _)),
            Type::Range => Value::Range(Range::from_raw(cvalue.u.r as *mut _)),
        }
    }

    pub fn cval(&self) -> c::_FcValue {
        c::_FcValue {
            type_: self.tag() as c::FcType,
            u: match *self {
                // Zig leaves these undefined.
                Value::Unknown | Value::Void => c::_FcValue__bindgen_ty_1 { i: 0 },
                Value::Integer(v) => c::_FcValue__bindgen_ty_1 { i: v as c_int },
                Value::Double(v) => c::_FcValue__bindgen_ty_1 { d: v },
                Value::String(v) => c::_FcValue__bindgen_ty_1 {
                    s: v.as_ptr() as *const c::FcChar8,
                },
                Value::Bool(v) => c::_FcValue__bindgen_ty_1 {
                    b: if v { c::FcTrue } else { c::FcFalse } as c::FcBool,
                },
                Value::Matrix(v) => c::_FcValue__bindgen_ty_1 {
                    m: v as *const Matrix as *const c::FcMatrix,
                },
                Value::CharSet(v) => c::_FcValue__bindgen_ty_1 { c: v.cval_const() },
                Value::FtFace(v) => c::_FcValue__bindgen_ty_1 { f: v },
                Value::LangSet(v) => c::_FcValue__bindgen_ty_1 { l: v.cval_const() },
                Value::Range(v) => c::_FcValue__bindgen_ty_1 { r: v.cval() },
            },
        }
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueBinding {
    Weak = c::FcValueBindingWeak,
    Strong = c::FcValueBindingStrong,
    Same = c::FcValueBindingSame,
}

impl ValueBinding {
    /// `@enumFromInt` for a raw `FcValueBinding`.
    pub fn from_raw(v: c::FcValueBinding) -> ValueBinding {
        match v {
            c::FcValueBindingWeak => ValueBinding::Weak,
            c::FcValueBindingStrong => ValueBinding::Strong,
            c::FcValueBindingSame => ValueBinding::Same,
            _ => unreachable!("invalid FcValueBinding {v}"),
        }
    }
}
