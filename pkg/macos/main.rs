//! Port of `src/macos/main.zig`.
//!
//! STATUS: ported
//!
//! Bindings for the Apple frameworks Ghostty uses (CoreFoundation,
//! CoreGraphics, CoreText, CoreVideo, QuartzCore, IOSurface, Carbon,
//! dispatch, os_log/os_signpost). The Zig package generates the raw
//! declarations with translate-c (`macos_c`); here they are hand-written in
//! [`c`] and the wrappers mirror the Zig ones.
//!
//! The wrapper types are opaque (`#[repr(C)]`, zero-sized) and are only ever
//! used behind references obtained from the frameworks. Like the Zig API,
//! Core Foundation objects are manually reference counted: a reference
//! returned by a `create*`/`copy*` function owns a +1 reference that must be
//! released exactly once (`release()`), after which it must not be used.
//! These references are typed `&'static` because their lifetime is
//! governed by the reference count, not by Rust scopes.

#![cfg(target_os = "macos")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "animation.rs"]
pub mod animation;
#[path = "carbon.rs"]
pub mod carbon;
#[path = "dispatch.rs"]
pub mod dispatch;
#[path = "foundation.rs"]
pub mod foundation;
#[path = "graphics.rs"]
pub mod graphics;
#[path = "iosurface.rs"]
pub mod iosurface;
#[path = "os.rs"]
pub mod os;
#[path = "text.rs"]
pub mod text;
#[path = "video.rs"]
pub mod video;
// @generated-mods-end

use std::fmt;

/// Errors returned by the wrappers (the Zig code uses `Allocator.Error`
/// and a few named error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    /// `Framesetter.createFrame`
    FrameCreateFailed,
    /// `DisplayLink.Error`
    CreationFailed,
    /// `DisplayLink.Error`, `IOSurface.Error`
    InvalidOperation,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::OutOfMemory => "OutOfMemory",
            Error::FrameCreateFailed => "FrameCreateFailed",
            Error::CreationFailed => "CreationFailed",
            Error::InvalidOperation => "InvalidOperation",
        })
    }
}

impl std::error::Error for Error {}

/// Implemented by every opaque framework object type in this package.
pub trait CFType {
    /// The raw object pointer (`@ptrCast(self)`).
    fn as_ptr(&self) -> *mut std::ffi::c_void;
}

/// Declare an opaque framework object type.
macro_rules! opaque_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[repr(C)]
        pub struct $name {
            _private: [u8; 0],
            _marker: ::std::marker::PhantomData<(*mut u8, ::std::marker::PhantomPinned)>,
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}({:p})", stringify!($name), self as *const Self)
            }
        }

        impl $crate::pkg::macos::CFType for $name {
            #[inline]
            fn as_ptr(&self) -> *mut ::std::ffi::c_void {
                self as *const Self as *mut ::std::ffi::c_void
            }
        }

        impl $name {
            /// Reinterpret a raw framework pointer as this type
            /// (`@ptrCast`), returning `None` for null.
            ///
            /// # Safety
            /// `ptr` must be null or point to a live object of this type.
            #[inline]
            pub unsafe fn from_ptr<'a>(ptr: *const ::std::ffi::c_void) -> Option<&'a Self> {
                // SAFETY: guaranteed by the caller.
                unsafe { (ptr as *const Self).as_ref() }
            }

            /// The raw object pointer.
            #[inline]
            pub fn as_ptr(&self) -> *mut ::std::ffi::c_void {
                self as *const Self as *mut ::std::ffi::c_void
            }
        }
    };
}
pub(crate) use opaque_type;

/// Convert a +1 pointer from a `Create`/`Copy` function into a reference,
/// mapping null to [`Error::OutOfMemory`].
///
/// # Safety
/// `ptr` must be null or point to a live object of type `T`.
#[inline]
pub(crate) unsafe fn created<T>(ptr: *const std::ffi::c_void) -> Result<&'static T, Error> {
    // SAFETY: guaranteed by the caller.
    unsafe { (ptr as *const T).as_ref() }.ok_or(Error::OutOfMemory)
}

/// Like [`created`] but returns `None` for null.
///
/// # Safety
/// `ptr` must be null or point to a live object of type `T`.
#[inline]
pub(crate) unsafe fn maybe<T>(ptr: *const std::ffi::c_void) -> Option<&'static T> {
    // SAFETY: guaranteed by the caller.
    unsafe { (ptr as *const T).as_ref() }
}

/// Hand-written declarations of the C APIs used by this package (the Zig
/// package's translate-c'd `macos_c`).
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code
)]
pub mod c {
    use std::ffi::{c_char, c_int, c_uint, c_void};

    // ---------------------------------------------------------------
    // CoreFoundation
    // ---------------------------------------------------------------

    pub type Boolean = u8;
    pub type CFIndex = isize;
    pub type CFOptionFlags = usize;
    pub type CFTypeRef = *const c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFMutableStringRef = *mut c_void;
    pub type CFArrayRef = *const c_void;
    pub type CFMutableArrayRef = *mut c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type CFMutableDictionaryRef = *mut c_void;
    pub type CFDataRef = *const c_void;
    pub type CFNumberRef = *const c_void;
    pub type CFURLRef = *const c_void;
    pub type CFAttributedStringRef = *const c_void;
    pub type CFMutableAttributedStringRef = *mut c_void;
    pub type CFCharacterSetRef = *const c_void;
    pub type CFStringEncoding = u32;
    pub type CFNumberType = CFIndex;
    pub type CFComparisonResult = CFIndex;
    pub type CFStringCompareFlags = CFOptionFlags;
    pub type CFURLPathStyle = CFIndex;
    pub type UniChar = u16;
    pub type UTF32Char = u32;
    pub type OSType = u32;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CFRange {
        pub location: CFIndex,
        pub length: CFIndex,
    }

    /// `CFArrayCallBacks` (only its address is used).
    #[repr(C)]
    pub struct CFArrayCallBacks {
        _private: [u8; 0],
    }

    /// `CFDictionaryKeyCallBacks` (only its address is used).
    #[repr(C)]
    pub struct CFDictionaryKeyCallBacks {
        _private: [u8; 0],
    }

    /// `CFDictionaryValueCallBacks` (only its address is used).
    #[repr(C)]
    pub struct CFDictionaryValueCallBacks {
        _private: [u8; 0],
    }

    pub type CFComparatorFunction = Option<
        unsafe extern "C" fn(
            val1: *const c_void,
            val2: *const c_void,
            context: *mut c_void,
        ) -> CFComparisonResult,
    >;

    pub const kCFNumberSInt8Type: CFNumberType = 1;
    pub const kCFNumberSInt16Type: CFNumberType = 2;
    pub const kCFNumberSInt32Type: CFNumberType = 3;
    pub const kCFNumberSInt64Type: CFNumberType = 4;
    pub const kCFNumberFloat32Type: CFNumberType = 5;
    pub const kCFNumberFloat64Type: CFNumberType = 6;
    pub const kCFNumberCharType: CFNumberType = 7;
    pub const kCFNumberShortType: CFNumberType = 8;
    pub const kCFNumberIntType: CFNumberType = 9;
    pub const kCFNumberLongType: CFNumberType = 10;
    pub const kCFNumberLongLongType: CFNumberType = 11;
    pub const kCFNumberFloatType: CFNumberType = 12;
    pub const kCFNumberDoubleType: CFNumberType = 13;
    pub const kCFNumberCFIndexType: CFNumberType = 14;
    pub const kCFNumberNSIntegerType: CFNumberType = 15;
    pub const kCFNumberCGFloatType: CFNumberType = 16;

    pub const kCFURLPOSIXPathStyle: CFURLPathStyle = 0;
    pub const kCFURLWindowsPathStyle: CFURLPathStyle = 2;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub static kCFAllocatorNull: CFAllocatorRef;
        pub static kCFNull: CFTypeRef;
        pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
        pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
        pub static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
        pub static kCFURLIsPurgeableKey: CFStringRef;
        pub static kCFURLIsVolumeKey: CFStringRef;

        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;

        pub fn CFArrayCreate(
            allocator: CFAllocatorRef,
            values: *const *const c_void,
            num_values: CFIndex,
            callbacks: *const CFArrayCallBacks,
        ) -> CFArrayRef;
        pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
        pub fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> *const c_void;
        pub fn CFArrayCreateMutable(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            callbacks: *const CFArrayCallBacks,
        ) -> CFMutableArrayRef;
        pub fn CFArrayCreateMutableCopy(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            array: CFArrayRef,
        ) -> CFMutableArrayRef;
        pub fn CFArrayAppendValue(array: CFMutableArrayRef, value: *const c_void);
        pub fn CFArrayRemoveValueAtIndex(array: CFMutableArrayRef, idx: CFIndex);
        pub fn CFArraySortValues(
            array: CFMutableArrayRef,
            range: CFRange,
            comparator: CFComparatorFunction,
            context: *mut c_void,
        );

        pub fn CFAttributedStringCreate(
            alloc: CFAllocatorRef,
            str: CFStringRef,
            attributes: CFDictionaryRef,
        ) -> CFAttributedStringRef;
        pub fn CFAttributedStringGetLength(astr: CFAttributedStringRef) -> CFIndex;
        pub fn CFAttributedStringGetString(astr: CFAttributedStringRef) -> CFStringRef;
        pub fn CFAttributedStringCreateMutable(
            alloc: CFAllocatorRef,
            max_length: CFIndex,
        ) -> CFMutableAttributedStringRef;
        pub fn CFAttributedStringReplaceString(
            astr: CFMutableAttributedStringRef,
            range: CFRange,
            replacement: CFStringRef,
        );
        pub fn CFAttributedStringSetAttribute(
            astr: CFMutableAttributedStringRef,
            range: CFRange,
            attr_name: CFStringRef,
            value: CFTypeRef,
        );

        pub fn CFCharacterSetCreateWithCharactersInString(
            alloc: CFAllocatorRef,
            the_string: CFStringRef,
        ) -> CFCharacterSetRef;
        pub fn CFCharacterSetCreateWithCharactersInRange(
            alloc: CFAllocatorRef,
            the_range: CFRange,
        ) -> CFCharacterSetRef;

        pub fn CFDataCreateWithBytesNoCopy(
            allocator: CFAllocatorRef,
            bytes: *const u8,
            length: CFIndex,
            bytes_deallocator: CFAllocatorRef,
        ) -> CFDataRef;
        pub fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
        pub fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;

        pub fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CFIndex,
            key_callbacks: *const CFDictionaryKeyCallBacks,
            value_callbacks: *const CFDictionaryValueCallBacks,
        ) -> CFDictionaryRef;
        pub fn CFDictionaryGetCount(the_dict: CFDictionaryRef) -> CFIndex;
        pub fn CFDictionaryGetValue(the_dict: CFDictionaryRef, key: *const c_void)
            -> *const c_void;
        pub fn CFDictionaryGetKeysAndValues(
            the_dict: CFDictionaryRef,
            keys: *mut *const c_void,
            values: *mut *const c_void,
        );
        pub fn CFDictionaryCreateMutable(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            key_callbacks: *const CFDictionaryKeyCallBacks,
            value_callbacks: *const CFDictionaryValueCallBacks,
        ) -> CFMutableDictionaryRef;
        pub fn CFDictionaryCreateMutableCopy(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            the_dict: CFDictionaryRef,
        ) -> CFMutableDictionaryRef;
        pub fn CFDictionarySetValue(
            the_dict: CFMutableDictionaryRef,
            key: *const c_void,
            value: *const c_void,
        );

        pub fn CFNumberCreate(
            allocator: CFAllocatorRef,
            the_type: CFNumberType,
            value_ptr: *const c_void,
        ) -> CFNumberRef;
        pub fn CFNumberGetValue(
            number: CFNumberRef,
            the_type: CFNumberType,
            value_ptr: *mut c_void,
        ) -> Boolean;

        pub fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: CFStringEncoding,
            is_external_representation: Boolean,
        ) -> CFStringRef;
        pub fn CFStringCreateWithCharactersNoCopy(
            alloc: CFAllocatorRef,
            chars: *const UniChar,
            num_chars: CFIndex,
            contents_deallocator: CFAllocatorRef,
        ) -> CFStringRef;
        pub fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
        pub fn CFStringHasPrefix(the_string: CFStringRef, prefix: CFStringRef) -> Boolean;
        pub fn CFStringCompare(
            the_string1: CFStringRef,
            the_string2: CFStringRef,
            compare_options: CFStringCompareFlags,
        ) -> CFComparisonResult;
        pub fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: CFIndex,
            encoding: CFStringEncoding,
        ) -> Boolean;
        pub fn CFStringGetCStringPtr(
            the_string: CFStringRef,
            encoding: CFStringEncoding,
        ) -> *const c_char;
        pub fn CFStringCreateMutable(
            alloc: CFAllocatorRef,
            max_length: CFIndex,
        ) -> CFMutableStringRef;
        pub fn CFStringAppendCharacters(
            the_string: CFMutableStringRef,
            chars: *const UniChar,
            num_chars: CFIndex,
        );

        pub fn CFURLCreateWithString(
            allocator: CFAllocatorRef,
            url_string: CFStringRef,
            base_url: CFURLRef,
        ) -> CFURLRef;
        pub fn CFURLCreateWithFileSystemPath(
            allocator: CFAllocatorRef,
            file_path: CFStringRef,
            path_style: CFURLPathStyle,
            is_directory: Boolean,
        ) -> CFURLRef;
        pub fn CFURLCreateStringByReplacingPercentEscapes(
            allocator: CFAllocatorRef,
            original_string: CFStringRef,
            chars_to_leave_escaped: CFStringRef,
        ) -> CFStringRef;
        pub fn CFURLCopyPath(an_url: CFURLRef) -> CFStringRef;
    }

    /// `CFStringGetSurrogatePairForLongCharacter` is a `CF_INLINE` function
    /// in the header (not exported), so it is implemented here.
    ///
    /// # Safety
    /// `surrogates` must point to at least two writable `UniChar`s.
    pub unsafe fn CFStringGetSurrogatePairForLongCharacter(
        character: UTF32Char,
        surrogates: *mut UniChar,
    ) -> Boolean {
        if character > 0xFFFF && character < 0x110000 {
            let ch = character - 0x10000;
            // SAFETY: guaranteed by the caller.
            unsafe {
                *surrogates = ((ch >> 10) + 0xD800) as UniChar;
                *surrogates.add(1) = ((ch & 0x3FF) + 0xDC00) as UniChar;
            }
            1
        } else {
            // SAFETY: guaranteed by the caller.
            unsafe { *surrogates = character as UniChar };
            0
        }
    }

    // ---------------------------------------------------------------
    // CoreGraphics
    // ---------------------------------------------------------------

    pub type CGFloat = f64;
    pub type CGGlyph = u16;
    pub type CGContextRef = *mut c_void;
    pub type CGColorSpaceRef = *mut c_void;
    pub type CGPathRef = *const c_void;
    pub type CGMutablePathRef = *mut c_void;
    pub type CGBitmapInfo = u32;
    pub type CGImageAlphaInfo = u32;
    pub type CGTextDrawingMode = i32;
    pub type CGDirectDisplayID = u32;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct CGPoint {
        pub x: CGFloat,
        pub y: CGFloat,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct CGSize {
        pub width: CGFloat,
        pub height: CGFloat,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct CGAffineTransform {
        pub a: CGFloat,
        pub b: CGFloat,
        pub c: CGFloat,
        pub d: CGFloat,
        pub tx: CGFloat,
        pub ty: CGFloat,
    }

    pub const kCGImageAlphaNone: CGImageAlphaInfo = 0;
    pub const kCGImageAlphaPremultipliedLast: CGImageAlphaInfo = 1;
    pub const kCGImageAlphaPremultipliedFirst: CGImageAlphaInfo = 2;
    pub const kCGImageAlphaLast: CGImageAlphaInfo = 3;
    pub const kCGImageAlphaFirst: CGImageAlphaInfo = 4;
    pub const kCGImageAlphaNoneSkipLast: CGImageAlphaInfo = 5;
    pub const kCGImageAlphaNoneSkipFirst: CGImageAlphaInfo = 6;
    pub const kCGImageAlphaOnly: CGImageAlphaInfo = 7;

    pub const kCGBitmapAlphaInfoMask: CGBitmapInfo = 0x1F;
    pub const kCGBitmapFloatInfoMask: CGBitmapInfo = 0xF00;
    pub const kCGBitmapFloatComponents: CGBitmapInfo = 1 << 8;
    pub const kCGBitmapByteOrderMask: CGBitmapInfo = 0x7000;
    pub const kCGBitmapByteOrderDefault: CGBitmapInfo = 0 << 12;
    pub const kCGBitmapByteOrder16Little: CGBitmapInfo = 1 << 12;
    pub const kCGBitmapByteOrder32Little: CGBitmapInfo = 2 << 12;
    pub const kCGBitmapByteOrder16Big: CGBitmapInfo = 3 << 12;
    pub const kCGBitmapByteOrder32Big: CGBitmapInfo = 4 << 12;

    pub const kCGTextFill: CGTextDrawingMode = 0;
    pub const kCGTextStroke: CGTextDrawingMode = 1;
    pub const kCGTextFillStroke: CGTextDrawingMode = 2;
    pub const kCGTextInvisible: CGTextDrawingMode = 3;
    pub const kCGTextFillClip: CGTextDrawingMode = 4;
    pub const kCGTextStrokeClip: CGTextDrawingMode = 5;
    pub const kCGTextFillStrokeClip: CGTextDrawingMode = 6;
    pub const kCGTextClip: CGTextDrawingMode = 7;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub static CGAffineTransformIdentity: CGAffineTransform;

        pub static kCGColorSpaceDisplayP3: CFStringRef;
        pub static kCGColorSpaceExtendedLinearDisplayP3: CFStringRef;
        pub static kCGColorSpaceSRGB: CFStringRef;
        pub static kCGColorSpaceExtendedSRGB: CFStringRef;
        pub static kCGColorSpaceLinearSRGB: CFStringRef;
        pub static kCGColorSpaceExtendedLinearSRGB: CFStringRef;
        pub static kCGColorSpaceGenericGrayGamma2_2: CFStringRef;
        pub static kCGColorSpaceExtendedGray: CFStringRef;
        pub static kCGColorSpaceLinearGray: CFStringRef;
        pub static kCGColorSpaceExtendedLinearGray: CFStringRef;

        pub fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> CGContextRef;

        pub fn CGColorSpaceCreateDeviceGray() -> CGColorSpaceRef;
        pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        pub fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;
        pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
        pub fn CGColorSpaceCopyPropertyList(space: CGColorSpaceRef) -> CFTypeRef;

        pub fn CGContextRelease(c: CGContextRef);
        pub fn CGContextSetLineWidth(c: CGContextRef, width: CGFloat);
        pub fn CGContextSetAllowsAntialiasing(c: CGContextRef, allows: bool);
        pub fn CGContextSetAllowsFontSmoothing(c: CGContextRef, allows: bool);
        pub fn CGContextSetAllowsFontSubpixelPositioning(c: CGContextRef, allows: bool);
        pub fn CGContextSetAllowsFontSubpixelQuantization(c: CGContextRef, allows: bool);
        pub fn CGContextSetShouldAntialias(c: CGContextRef, should: bool);
        pub fn CGContextSetShouldSmoothFonts(c: CGContextRef, should: bool);
        pub fn CGContextSetShouldSubpixelPositionFonts(c: CGContextRef, should: bool);
        pub fn CGContextSetShouldSubpixelQuantizeFonts(c: CGContextRef, should: bool);
        pub fn CGContextSetGrayFillColor(c: CGContextRef, gray: CGFloat, alpha: CGFloat);
        pub fn CGContextSetGrayStrokeColor(c: CGContextRef, gray: CGFloat, alpha: CGFloat);
        pub fn CGContextSetRGBFillColor(
            c: CGContextRef,
            r: CGFloat,
            g: CGFloat,
            b: CGFloat,
            a: CGFloat,
        );
        pub fn CGContextSetRGBStrokeColor(
            c: CGContextRef,
            r: CGFloat,
            g: CGFloat,
            b: CGFloat,
            a: CGFloat,
        );
        pub fn CGContextSetTextDrawingMode(c: CGContextRef, mode: CGTextDrawingMode);
        pub fn CGContextSetTextMatrix(c: CGContextRef, t: CGAffineTransform);
        pub fn CGContextSetTextPosition(c: CGContextRef, x: CGFloat, y: CGFloat);
        pub fn CGContextFillRect(c: CGContextRef, rect: CGRect);
        pub fn CGContextScaleCTM(c: CGContextRef, sx: CGFloat, sy: CGFloat);
        pub fn CGContextTranslateCTM(c: CGContextRef, tx: CGFloat, ty: CGFloat);

        pub fn CGRectIsNull(rect: CGRect) -> bool;
        pub fn CGRectGetHeight(rect: CGRect) -> CGFloat;
        pub fn CGRectGetWidth(rect: CGRect) -> CGFloat;

        pub fn CGPathCreateWithRect(rect: CGRect, transform: *const CGAffineTransform)
            -> CGPathRef;
        pub fn CGPathCreateMutable() -> CGMutablePathRef;
        pub fn CGPathAddRect(path: CGMutablePathRef, m: *const CGAffineTransform, rect: CGRect);
        pub fn CGPathGetBoundingBox(path: CGPathRef) -> CGRect;
    }

    // ---------------------------------------------------------------
    // CoreText
    // ---------------------------------------------------------------

    pub type CTFontRef = *const c_void;
    pub type CTFontDescriptorRef = *const c_void;
    pub type CTFontCollectionRef = *const c_void;
    pub type CTFrameRef = *const c_void;
    pub type CTFramesetterRef = *const c_void;
    pub type CTLineRef = *const c_void;
    pub type CTRunRef = *const c_void;
    pub type CTTypesetterRef = *const c_void;
    pub type CTParagraphStyleRef = *const c_void;
    pub type CTFontOrientation = u32;
    pub type CTFontTableTag = u32;
    pub type CTFontTableOptions = u32;
    pub type CTFontSymbolicTraits = u32;
    pub type CTLineBoundsOptions = CFOptionFlags;
    pub type CTRunStatus = u32;

    pub const kCTFontOrientationDefault: CTFontOrientation = 0;
    pub const kCTFontOrientationHorizontal: CTFontOrientation = 1;
    pub const kCTFontOrientationVertical: CTFontOrientation = 2;

    pub const kCTFontTableOptionNoOptions: CTFontTableOptions = 0;

    const fn four_char_code(v: &[u8; 4]) -> u32 {
        ((v[0] as u32) << 24) | ((v[1] as u32) << 16) | ((v[2] as u32) << 8) | (v[3] as u32)
    }

    pub const kCTFontTableSVG: CTFontTableTag = four_char_code(b"SVG ");
    pub const kCTFontTableOS2: CTFontTableTag = four_char_code(b"OS/2");
    pub const kCTFontTableHead: CTFontTableTag = four_char_code(b"head");
    pub const kCTFontTableHhea: CTFontTableTag = four_char_code(b"hhea");
    pub const kCTFontTablePost: CTFontTableTag = four_char_code(b"post");

    pub const kCTFontTraitItalic: CTFontSymbolicTraits = 1 << 0;
    pub const kCTFontTraitBold: CTFontSymbolicTraits = 1 << 1;
    pub const kCTFontTraitExpanded: CTFontSymbolicTraits = 1 << 5;
    pub const kCTFontTraitCondensed: CTFontSymbolicTraits = 1 << 6;
    pub const kCTFontTraitMonoSpace: CTFontSymbolicTraits = 1 << 10;
    pub const kCTFontTraitVertical: CTFontSymbolicTraits = 1 << 11;
    pub const kCTFontTraitUIOptimized: CTFontSymbolicTraits = 1 << 12;
    pub const kCTFontTraitColorGlyphs: CTFontSymbolicTraits = 1 << 13;
    pub const kCTFontTraitComposite: CTFontSymbolicTraits = 1 << 14;

    pub const kCTLineBoundsExcludeTypographicLeading: CTLineBoundsOptions = 1 << 0;
    pub const kCTLineBoundsExcludeTypographicShifts: CTLineBoundsOptions = 1 << 1;
    pub const kCTLineBoundsUseHangingPunctuation: CTLineBoundsOptions = 1 << 2;
    pub const kCTLineBoundsUseGlyphPathBounds: CTLineBoundsOptions = 1 << 3;
    pub const kCTLineBoundsUseOpticalBounds: CTLineBoundsOptions = 1 << 4;
    pub const kCTLineBoundsIncludeLanguageExtents: CTLineBoundsOptions = 1 << 5;

    pub const kCTRunStatusNoStatus: CTRunStatus = 0;
    pub const kCTRunStatusRightToLeft: CTRunStatus = 1 << 0;
    pub const kCTRunStatusNonMonotonic: CTRunStatus = 1 << 1;
    pub const kCTRunStatusHasNonIdentityMatrix: CTRunStatus = 1 << 2;

    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        pub static kCTFontURLAttribute: CFStringRef;
        pub static kCTFontNameAttribute: CFStringRef;
        pub static kCTFontDisplayNameAttribute: CFStringRef;
        pub static kCTFontFamilyNameAttribute: CFStringRef;
        pub static kCTFontStyleNameAttribute: CFStringRef;
        pub static kCTFontTraitsAttribute: CFStringRef;
        pub static kCTFontVariationAttribute: CFStringRef;
        pub static kCTFontSizeAttribute: CFStringRef;
        pub static kCTFontMatrixAttribute: CFStringRef;
        pub static kCTFontCascadeListAttribute: CFStringRef;
        pub static kCTFontCharacterSetAttribute: CFStringRef;
        pub static kCTFontLanguagesAttribute: CFStringRef;
        pub static kCTFontBaselineAdjustAttribute: CFStringRef;
        pub static kCTFontMacintoshEncodingsAttribute: CFStringRef;
        pub static kCTFontFeaturesAttribute: CFStringRef;
        pub static kCTFontFeatureSettingsAttribute: CFStringRef;
        pub static kCTFontFixedAdvanceAttribute: CFStringRef;
        pub static kCTFontOrientationAttribute: CFStringRef;
        pub static kCTFontFormatAttribute: CFStringRef;
        pub static kCTFontRegistrationScopeAttribute: CFStringRef;
        pub static kCTFontPriorityAttribute: CFStringRef;
        pub static kCTFontEnabledAttribute: CFStringRef;
        pub static kCTFontDownloadableAttribute: CFStringRef;
        pub static kCTFontDownloadedAttribute: CFStringRef;
        pub static kCTFontVariationAxesAttribute: CFStringRef;

        pub static kCTFontSymbolicTrait: CFStringRef;
        pub static kCTFontWeightTrait: CFStringRef;
        pub static kCTFontWidthTrait: CFStringRef;
        pub static kCTFontSlantTrait: CFStringRef;

        pub static kCTFontVariationAxisIdentifierKey: CFStringRef;
        pub static kCTFontVariationAxisMinimumValueKey: CFStringRef;
        pub static kCTFontVariationAxisMaximumValueKey: CFStringRef;
        pub static kCTFontVariationAxisDefaultValueKey: CFStringRef;
        pub static kCTFontVariationAxisNameKey: CFStringRef;
        pub static kCTFontVariationAxisHiddenKey: CFStringRef;

        pub static kCTFontAttributeName: CFStringRef;
        pub static kCTParagraphStyleAttributeName: CFStringRef;
        pub static kCTWritingDirectionAttributeName: CFStringRef;

        pub static kCTFontOpenTypeFeatureTag: CFStringRef;
        pub static kCTFontOpenTypeFeatureValue: CFStringRef;

        pub static kCTTypesetterOptionForcedEmbeddingLevel: CFStringRef;

        pub fn CTFontCreateWithFontDescriptor(
            descriptor: CTFontDescriptorRef,
            size: CGFloat,
            matrix: *const CGAffineTransform,
        ) -> CTFontRef;
        pub fn CTFontCreateWithName(
            name: CFStringRef,
            size: CGFloat,
            matrix: *const CGAffineTransform,
        ) -> CTFontRef;
        pub fn CTFontCreateForString(
            current_font: CTFontRef,
            string: CFStringRef,
            range: CFRange,
        ) -> CTFontRef;
        pub fn CTFontCreateCopyWithAttributes(
            font: CTFontRef,
            size: CGFloat,
            matrix: *const CGAffineTransform,
            attributes: CTFontDescriptorRef,
        ) -> CTFontRef;
        pub fn CTFontCopyFontDescriptor(font: CTFontRef) -> CTFontDescriptorRef;
        pub fn CTFontCopyFeatures(font: CTFontRef) -> CFArrayRef;
        pub fn CTFontCopyDefaultCascadeListForLanguages(
            font: CTFontRef,
            language_pref_list: CFArrayRef,
        ) -> CFArrayRef;
        pub fn CTFontCopyTable(
            font: CTFontRef,
            table: CTFontTableTag,
            options: CTFontTableOptions,
        ) -> CFDataRef;
        pub fn CTFontGetGlyphCount(font: CTFontRef) -> CFIndex;
        pub fn CTFontGetGlyphsForCharacters(
            font: CTFontRef,
            characters: *const UniChar,
            glyphs: *mut CGGlyph,
            count: CFIndex,
        ) -> bool;
        pub fn CTFontCreatePathForGlyph(
            font: CTFontRef,
            glyph: CGGlyph,
            matrix: *const CGAffineTransform,
        ) -> CGPathRef;
        pub fn CTFontDrawGlyphs(
            font: CTFontRef,
            glyphs: *const CGGlyph,
            positions: *const CGPoint,
            count: usize,
            context: CGContextRef,
        );
        pub fn CTFontGetBoundingRectsForGlyphs(
            font: CTFontRef,
            orientation: CTFontOrientation,
            glyphs: *const CGGlyph,
            bounding_rects: *mut CGRect,
            count: CFIndex,
        ) -> CGRect;
        pub fn CTFontGetAdvancesForGlyphs(
            font: CTFontRef,
            orientation: CTFontOrientation,
            glyphs: *const CGGlyph,
            advances: *mut CGSize,
            count: CFIndex,
        ) -> f64;
        pub fn CTFontCopyAttribute(font: CTFontRef, attribute: CFStringRef) -> CFTypeRef;
        pub fn CTFontCopyFamilyName(font: CTFontRef) -> CFStringRef;
        pub fn CTFontCopyDisplayName(font: CTFontRef) -> CFStringRef;
        pub fn CTFontCopyPostScriptName(font: CTFontRef) -> CFStringRef;
        pub fn CTFontGetSymbolicTraits(font: CTFontRef) -> CTFontSymbolicTraits;
        pub fn CTFontGetAscent(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetDescent(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetLeading(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetBoundingBox(font: CTFontRef) -> CGRect;
        pub fn CTFontGetUnderlinePosition(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetUnderlineThickness(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetCapHeight(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetXHeight(font: CTFontRef) -> CGFloat;
        pub fn CTFontGetUnitsPerEm(font: CTFontRef) -> c_uint;
        pub fn CTFontGetSize(font: CTFontRef) -> CGFloat;

        pub fn CTFontCollectionCreateFromAvailableFonts(
            options: CFDictionaryRef,
        ) -> CTFontCollectionRef;
        pub fn CTFontCollectionCreateWithFontDescriptors(
            query_descriptors: CFArrayRef,
            options: CFDictionaryRef,
        ) -> CTFontCollectionRef;
        pub fn CTFontCollectionCreateMatchingFontDescriptors(
            collection: CTFontCollectionRef,
        ) -> CFArrayRef;

        pub fn CTFontDescriptorCreateWithNameAndSize(
            name: CFStringRef,
            size: CGFloat,
        ) -> CTFontDescriptorRef;
        pub fn CTFontDescriptorCreateWithAttributes(
            attributes: CFDictionaryRef,
        ) -> CTFontDescriptorRef;
        pub fn CTFontDescriptorCreateCopyWithAttributes(
            original: CTFontDescriptorRef,
            attributes: CFDictionaryRef,
        ) -> CTFontDescriptorRef;
        pub fn CTFontDescriptorCreateCopyWithVariation(
            original: CTFontDescriptorRef,
            variation_identifier: CFNumberRef,
            variation_value: CGFloat,
        ) -> CTFontDescriptorRef;
        pub fn CTFontDescriptorCopyAttribute(
            descriptor: CTFontDescriptorRef,
            attribute: CFStringRef,
        ) -> CFTypeRef;
        pub fn CTFontDescriptorCopyAttributes(descriptor: CTFontDescriptorRef) -> CFDictionaryRef;

        pub fn CTFontManagerCreateFontDescriptorsFromURL(file_url: CFURLRef) -> CFArrayRef;
        pub fn CTFontManagerCreateFontDescriptorsFromData(data: CFDataRef) -> CFArrayRef;
        pub fn CTFontManagerCreateFontDescriptorFromData(data: CFDataRef) -> CTFontDescriptorRef;

        pub fn CTFrameGetLineOrigins(frame: CTFrameRef, range: CFRange, origins: *mut CGPoint);
        pub fn CTFrameGetLines(frame: CTFrameRef) -> CFArrayRef;
        pub fn CTFramesetterCreateWithAttributedString(
            attr_string: CFAttributedStringRef,
        ) -> CTFramesetterRef;
        pub fn CTFramesetterCreateFrame(
            framesetter: CTFramesetterRef,
            string_range: CFRange,
            path: CGPathRef,
            frame_attributes: CFDictionaryRef,
        ) -> CTFrameRef;

        pub fn CTLineCreateWithAttributedString(attr_string: CFAttributedStringRef) -> CTLineRef;
        pub fn CTLineGetGlyphCount(line: CTLineRef) -> CFIndex;
        pub fn CTLineGetBoundsWithOptions(line: CTLineRef, options: CTLineBoundsOptions) -> CGRect;
        pub fn CTLineGetTypographicBounds(
            line: CTLineRef,
            ascent: *mut CGFloat,
            descent: *mut CGFloat,
            leading: *mut CGFloat,
        ) -> f64;
        pub fn CTLineGetGlyphRuns(line: CTLineRef) -> CFArrayRef;

        pub fn CTParagraphStyleCreate(
            settings: *const c_void,
            setting_count: usize,
        ) -> CTParagraphStyleRef;

        pub fn CTRunGetGlyphCount(run: CTRunRef) -> CFIndex;
        pub fn CTRunGetGlyphsPtr(run: CTRunRef) -> *const CGGlyph;
        pub fn CTRunGetGlyphs(run: CTRunRef, range: CFRange, buffer: *mut CGGlyph);
        pub fn CTRunGetPositionsPtr(run: CTRunRef) -> *const CGPoint;
        pub fn CTRunGetPositions(run: CTRunRef, range: CFRange, buffer: *mut CGPoint);
        pub fn CTRunGetAdvancesPtr(run: CTRunRef) -> *const CGSize;
        pub fn CTRunGetAdvances(run: CTRunRef, range: CFRange, buffer: *mut CGSize);
        pub fn CTRunGetStringIndicesPtr(run: CTRunRef) -> *const CFIndex;
        pub fn CTRunGetStringIndices(run: CTRunRef, range: CFRange, buffer: *mut CFIndex);
        pub fn CTRunGetStatus(run: CTRunRef) -> CTRunStatus;

        pub fn CTTypesetterCreateWithAttributedStringAndOptions(
            string: CFAttributedStringRef,
            options: CFDictionaryRef,
        ) -> CTTypesetterRef;
        pub fn CTTypesetterCreateLine(
            typesetter: CTTypesetterRef,
            string_range: CFRange,
        ) -> CTLineRef;
    }

    // ---------------------------------------------------------------
    // CoreVideo
    // ---------------------------------------------------------------

    pub type CVReturn = i32;
    pub type CVOptionFlags = u64;
    pub type CVDisplayLinkRef = *mut c_void;

    pub const kCVReturnSuccess: CVReturn = 0;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub type_: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    pub type CVDisplayLinkOutputCallback = Option<
        unsafe extern "C" fn(
            display_link: CVDisplayLinkRef,
            in_now: *const CVTimeStamp,
            in_output_time: *const CVTimeStamp,
            flags_in: CVOptionFlags,
            flags_out: *mut CVOptionFlags,
            display_link_context: *mut c_void,
        ) -> CVReturn,
    >;

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(
            display_link_out: *mut CVDisplayLinkRef,
        ) -> CVReturn;
        pub fn CVDisplayLinkRelease(display_link: CVDisplayLinkRef);
        pub fn CVDisplayLinkStart(display_link: CVDisplayLinkRef) -> CVReturn;
        pub fn CVDisplayLinkStop(display_link: CVDisplayLinkRef) -> CVReturn;
        pub fn CVDisplayLinkIsRunning(display_link: CVDisplayLinkRef) -> Boolean;
        pub fn CVDisplayLinkSetCurrentCGDisplay(
            display_link: CVDisplayLinkRef,
            display_id: CGDirectDisplayID,
        ) -> CVReturn;
        pub fn CVDisplayLinkSetOutputCallback(
            display_link: CVDisplayLinkRef,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> CVReturn;
    }

    // ---------------------------------------------------------------
    // QuartzCore
    // ---------------------------------------------------------------

    #[link(name = "QuartzCore", kind = "framework")]
    extern "C" {}

    // ---------------------------------------------------------------
    // IOSurface
    // ---------------------------------------------------------------

    pub type IOSurfaceRef = *mut c_void;
    pub type IOSurfaceLockOptions = u32;
    pub type IOSurfacePurgeabilityState = u32;
    pub type kern_return_t = c_int;

    pub const kIOSurfacePurgeableNonVolatile: IOSurfacePurgeabilityState = 0;
    pub const kIOSurfacePurgeableVolatile: IOSurfacePurgeabilityState = 1;
    pub const kIOSurfacePurgeableEmpty: IOSurfacePurgeabilityState = 2;
    pub const kIOSurfacePurgeableKeepCurrent: IOSurfacePurgeabilityState = 3;

    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        pub static kIOSurfaceWidth: CFStringRef;
        pub static kIOSurfaceHeight: CFStringRef;
        pub static kIOSurfacePixelFormat: CFStringRef;
        pub static kIOSurfaceBytesPerElement: CFStringRef;
        pub static kIOSurfaceColorSpace: CFStringRef;

        pub fn IOSurfaceCreate(properties: CFDictionaryRef) -> IOSurfaceRef;
        pub fn IOSurfaceSetPurgeable(
            buffer: IOSurfaceRef,
            new_state: u32,
            old_state: *mut u32,
        ) -> kern_return_t;
        pub fn IOSurfaceSetValue(buffer: IOSurfaceRef, key: CFStringRef, value: CFTypeRef);
        pub fn IOSurfaceLock(
            buffer: IOSurfaceRef,
            options: IOSurfaceLockOptions,
            seed: *mut u32,
        ) -> kern_return_t;
        pub fn IOSurfaceUnlock(
            buffer: IOSurfaceRef,
            options: IOSurfaceLockOptions,
            seed: *mut u32,
        ) -> kern_return_t;
        pub fn IOSurfaceGetAllocSize(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetBytesPerElement(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetBytesPerRow(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetBaseAddress(buffer: IOSurfaceRef) -> *mut c_void;
        pub fn IOSurfaceGetElementWidth(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetElementHeight(buffer: IOSurfaceRef) -> usize;
        pub fn IOSurfaceGetPixelFormat(buffer: IOSurfaceRef) -> OSType;
    }

    // ---------------------------------------------------------------
    // Carbon (nothing is used directly; linked for completeness)
    // ---------------------------------------------------------------

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {}

    // ---------------------------------------------------------------
    // libSystem: dispatch, os_log, os_signpost
    // ---------------------------------------------------------------

    pub type dispatch_queue_t = *mut c_void;
    pub type dispatch_data_t = *mut c_void;
    pub type dispatch_block_t = *mut c_void;

    /// `DISPATCH_DATA_DESTRUCTOR_DEFAULT`
    pub const DISPATCH_DATA_DESTRUCTOR_DEFAULT: dispatch_block_t = std::ptr::null_mut();

    /// The main queue object (`struct dispatch_queue_s`); only its address is
    /// used.
    #[repr(C)]
    pub struct dispatch_queue_s {
        _private: [u8; 0],
    }

    pub type os_log_t = *mut c_void;
    pub type os_log_type_t = u8;
    pub type os_signpost_id_t = u64;
    pub type os_signpost_type_t = u8;

    pub const OS_LOG_TYPE_DEFAULT: os_log_type_t = 0x00;
    pub const OS_LOG_TYPE_INFO: os_log_type_t = 0x01;
    pub const OS_LOG_TYPE_DEBUG: os_log_type_t = 0x02;
    pub const OS_LOG_TYPE_ERROR: os_log_type_t = 0x10;
    pub const OS_LOG_TYPE_FAULT: os_log_type_t = 0x11;

    /// `struct mach_header` (only its address is used).
    #[repr(C)]
    pub struct mach_header {
        _private: [u8; 0],
    }

    extern "C" {
        pub static _dispatch_main_q: dispatch_queue_s;

        pub fn dispatch_data_create(
            buffer: *const c_void,
            size: usize,
            queue: dispatch_queue_t,
            destructor: dispatch_block_t,
        ) -> dispatch_data_t;

        pub fn os_log_create(subsystem: *const c_char, category: *const c_char) -> os_log_t;
        pub fn os_release(object: *mut c_void);
        pub fn os_log_type_enabled(oslog: os_log_t, type_: os_log_type_t) -> bool;

        pub fn os_signpost_enabled(log: os_log_t) -> bool;
        pub fn os_signpost_id_generate(log: os_log_t) -> os_signpost_id_t;
        pub fn os_signpost_id_make_with_pointer(
            log: os_log_t,
            ptr: *const c_void,
        ) -> os_signpost_id_t;
        pub fn _os_signpost_emit_with_name_impl(
            dso: *mut c_void,
            log: os_log_t,
            type_: os_signpost_type_t,
            spid: os_signpost_id_t,
            name: *const c_char,
            format: *const c_char,
            buf: *mut u8,
            size: u32,
        );
    }

    /// `dispatch_get_main_queue()` is a macro for `&_dispatch_main_q`.
    pub fn dispatch_get_main_queue() -> dispatch_queue_t {
        // SAFETY: taking the address of the main queue object.
        std::ptr::addr_of!(_dispatch_main_q) as dispatch_queue_t
    }
}
