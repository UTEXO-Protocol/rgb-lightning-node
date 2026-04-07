// Strict encoding library for deterministic binary serialization.
//
// SPDX-License-Identifier: Apache-2.0
//
// Designed in 2019-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
// Written in 2024-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
//
// Copyright (C) 2022-2025 Laboratories for Ubiquitous Deterministic Computing (UBIDECO),
//                         Institute for Distributed and Cognitive Systems (InDCS), Switzerland.
// Copyright (C) 2022-2025 Dr Maxim Orlovsky.
// All rights under the above copyrights are reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except
// in compliance with the License. You may obtain a copy of the License at
//
//        http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software distributed under the License
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express
// or implied. See the License for the specific language governing permissions and limitations under
// the License.

#![allow(non_camel_case_types, unused_imports)]

use std::borrow::Borrow;
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::ops::Deref;
use std::str::FromStr;
use std::{any, io};

use amplify::ascii::{AsAsciiStrError, AsciiChar, AsciiString, FromAsciiError};
use amplify::confinement;
use amplify::confinement::Confined;
use amplify::num::{u1, u2, u3, u4, u5, u6, u7};

use crate::{
    type_name, DecodeError, DefaultBasedStrictDumb, StrictDecode, StrictDumb, StrictEncode,
    StrictEnum, StrictSum, StrictType, TypeName, TypedRead, TypedWrite, VariantError, LIB_NAME_STD,
};

// TODO: Move RString and related ASCII types to amplify library

#[derive(Clone, Eq, PartialEq, Hash, Debug, Display, Error, From)]
#[display(doc_comments)]
pub enum InvalidRString {
    /// must contain at least one character.
    Empty,

    /// string '{0}' must not start with character '{1}'.
    DisallowedFirst(String, char),

    /// string '{0}' contains invalid character '{1}' at position {2}.
    InvalidChar(String, char, usize),

    #[from(AsAsciiStrError)]
    /// string contains non-ASCII character(s).
    NonAsciiChar,

    /// string has invalid length.
    #[from]
    Confinement(confinement::Error),
}

impl<O> From<FromAsciiError<O>> for InvalidRString {
    fn from(_: FromAsciiError<O>) -> Self { InvalidRString::NonAsciiChar }
}

pub trait RestrictedCharSet:
    Copy + Into<u8> + TryFrom<u8, Error = VariantError<u8>> + Display + StrictEncode + StrictDumb
{
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize), serde(crate = "serde_crate", transparent))]
pub struct RString<
    C1: RestrictedCharSet,
    C: RestrictedCharSet = C1,
    const MIN: usize = 1,
    const MAX: usize = 255,
> {
    s: Confined<AsciiString, MIN, MAX>,
    first: PhantomData<C1>,
    rest: PhantomData<C>,
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> Deref
    for RString<C1, C, MIN, MAX>
{
    type Target = AsciiString;
    fn deref(&self) -> &Self::Target { self.s.as_unconfined() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> AsRef<[u8]>
    for RString<C1, C, MIN, MAX>
{
    fn as_ref(&self) -> &[u8] { self.s.as_bytes() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> AsRef<str>
    for RString<C1, C, MIN, MAX>
{
    #[inline]
    fn as_ref(&self) -> &str { self.s.as_str() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> Borrow<str>
    for RString<C1, C, MIN, MAX>
{
    fn borrow(&self) -> &str { self.s.as_str() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> FromStr
    for RString<C1, C, MIN, MAX>
{
    type Err = InvalidRString;

    fn from_str(s: &str) -> Result<Self, Self::Err> { Self::try_from(s.as_bytes()) }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
    From<&'static str> for RString<C1, C, MIN, MAX>
{
    /// # Safety
    ///
    /// Panics if the string contains invalid characters not matching
    /// [`RestrictedCharSet`] requirements or the string length exceeds `MAX`.
    fn from(s: &'static str) -> Self {
        Self::try_from(s.as_bytes()).expect("invalid static string")
    }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
    TryFrom<String> for RString<C1, C, MIN, MAX>
{
    type Error = InvalidRString;

    fn try_from(s: String) -> Result<Self, Self::Error> { Self::try_from(s.as_bytes()) }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
    TryFrom<AsciiString> for RString<C1, C, MIN, MAX>
{
    type Error = InvalidRString;

    fn try_from(ascii: AsciiString) -> Result<Self, InvalidRString> { ascii.as_bytes().try_into() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
    TryFrom<Vec<u8>> for RString<C1, C, MIN, MAX>
{
    type Error = InvalidRString;

    fn try_from(vec: Vec<u8>) -> Result<Self, InvalidRString> { vec.as_slice().try_into() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> TryFrom<&[u8]>
    for RString<C1, C, MIN, MAX>
{
    type Error = InvalidRString;

    fn try_from(bytes: &[u8]) -> Result<Self, InvalidRString> {
        if bytes.is_empty() && MIN == 0 {
            return Ok(Self {
                s: Confined::from_checked(AsciiString::new()),
                first: PhantomData,
                rest: PhantomData,
            });
        }
        let utf8 = String::from_utf8_lossy(bytes);
        let mut iter = bytes.iter();
        let Some(first) = iter.next() else {
            return Err(InvalidRString::Empty);
        };
        if C1::try_from(*first).is_err() {
            return Err(InvalidRString::DisallowedFirst(
                utf8.to_string(),
                utf8.chars().next().unwrap_or('?'),
            ));
        }
        if let Some(pos) = iter.position(|ch| C::try_from(*ch).is_err()) {
            return Err(InvalidRString::InvalidChar(
                utf8.to_string(),
                utf8.chars().nth(pos + 1).unwrap_or('?'),
                pos + 1,
            ));
        }
        let s = Confined::try_from(
            AsciiString::from_ascii(bytes).expect("not an ASCII characted subset"),
        )?;
        Ok(Self {
            s,
            first: PhantomData,
            rest: PhantomData,
        })
    }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
    From<RString<C1, C, MIN, MAX>> for String
{
    fn from(s: RString<C1, C, MIN, MAX>) -> Self { s.s.release().into() }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> Debug
    for RString<C1, C, MIN, MAX>
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let c = type_name::<C>();
        let c1 = type_name::<C>();
        let c = if c == c1 { c.to_owned() } else { format!("{c1}, {c}") };
        f.debug_tuple(&format!("RString<{c}[{MIN}..{MAX}]>")).field(&self.as_str()).finish()
    }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize> Display
    for RString<C1, C, MIN, MAX>
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result { Display::fmt(&self.s, f) }
}

#[cfg(feature = "serde")]
mod _serde {
    use serde_crate::de::Error;
    use serde_crate::{Deserialize, Deserializer};

    use super::*;

    impl<'de, C1: RestrictedCharSet, C: RestrictedCharSet, const MIN: usize, const MAX: usize>
        Deserialize<'de> for RString<C1, C, MIN, MAX>
    {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: Deserializer<'de> {
            let ascii = AsciiString::deserialize(deserializer)?;
            Self::try_from(ascii).map_err(D::Error::custom)
        }
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
#[derive(StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum Bool {
    #[default]
    False = 0,
    True = 1,
}
impl DefaultBasedStrictDumb for Bool {}
impl From<&bool> for Bool {
    fn from(value: &bool) -> Self { Bool::from(*value) }
}
impl From<bool> for Bool {
    fn from(value: bool) -> Self {
        match value {
            true => Bool::True,
            false => Bool::False,
        }
    }
}
impl From<&Bool> for bool {
    fn from(value: &Bool) -> Self { bool::from(*value) }
}
impl From<Bool> for bool {
    fn from(value: Bool) -> Self {
        match value {
            Bool::False => false,
            Bool::True => true,
        }
    }
}

impl StrictType for bool {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_STD;
    fn strict_name() -> Option<TypeName> { Some(tn!("Bool")) }
}
impl StrictEncode for bool {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_enum::<Bool>(Bool::from(self))
    }
}
impl StrictDecode for bool {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let v: Bool = reader.read_enum()?;
        Ok(bool::from(v))
    }
}

macro_rules! impl_u {
    ($ty:ident, $inner:ty, $( $no:ident )+) => {
        #[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
        #[derive(StrictType, StrictEncode, StrictDecode)]
        #[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
        #[repr(u8)]
        pub enum $ty {
            #[default]
            $( $no ),+
        }

        impl DefaultBasedStrictDumb for $ty { }

        impl StrictType for $inner {
            const STRICT_LIB_NAME: &'static str = LIB_NAME_STD;
            fn strict_name() -> Option<TypeName> { Some(tn!(stringify!($ty))) }
        }
        impl StrictEncode for $inner {
            fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
                writer.write_enum::<$ty>($ty::try_from(self.to_u8())
                    .expect(concat!("broken", stringify!($inner), "type guarantees")))
            }
        }
        impl StrictDecode for $inner {
            fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
                let v: $ty = reader.read_enum()?;
                Ok(<$inner>::with(v as u8))
            }
        }
    }
}

impl_u!(U1, u1, _0 _1);
impl_u!(U2, u2, _0 _1 _2 _3);
impl_u!(U3, u3, _0 _1 _2 _3 _4 _5 _6 _7);
impl_u!(U4, u4, _0 _1 _2 _3 _4 _5 _6 _7 _8 _9 _10 _11 _12 _13 _14 _15);
impl_u!(U5, u5, _0 _1 _2 _3 _4 _5 _6 _7 _8 _9 
                _10 _11 _12 _13 _14 _15 _16 _17 _18 _19 
                _20 _21 _22 _23 _24 _25 _26 _27 _28 _29
                _30 _31);
impl_u!(U6, u6, _0 _1 _2 _3 _4 _5 _6 _7 _8 _9 
                _10 _11 _12 _13 _14 _15 _16 _17 _18 _19 
                _20 _21 _22 _23 _24 _25 _26 _27 _28 _29
                _30 _31 _32 _33 _34 _35 _36 _37 _38 _39
                _40 _41 _42 _43 _44 _45 _46 _47 _48 _49
                _50 _51 _52 _53 _54 _55 _56 _57 _58 _59
                _60 _61 _62 _63);
impl_u!(U7, u7, _0 _1 _2 _3 _4 _5 _6 _7 _8 _9 
                _10 _11 _12 _13 _14 _15 _16 _17 _18 _19 
                _20 _21 _22 _23 _24 _25 _26 _27 _28 _29
                _30 _31 _32 _33 _34 _35 _36 _37 _38 _39
                _40 _41 _42 _43 _44 _45 _46 _47 _48 _49
                _50 _51 _52 _53 _54 _55 _56 _57 _58 _59
                _60 _61 _62 _63 _64 _65 _66 _67 _68 _69
                _70 _71 _72 _73 _74 _75 _76 _77 _78 _79
                _80 _81 _82 _83 _84 _85 _86 _87 _88 _89
                _90 _91 _92 _93 _94 _95 _96 _97 _98 _99
                _100 _101 _102 _103 _104 _105 _106 _107 _108 _109
                _110 _111 _112 _113 _114 _115 _116 _117 _118 _119
                _120 _121 _122 _123 _124 _125 _126 _127);

#[derive(Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, Debug)]
#[wrapper_mut(DerefMut)]
#[derive(StrictDumb)]
#[strict_type(lib = LIB_NAME_STD, dumb = Self(AsciiChar::A), crate = crate)]
pub struct AsciiSym(AsciiChar);

impl From<AsciiSym> for u8 {
    fn from(value: AsciiSym) -> Self { value.0.as_byte() }
}

impl TryFrom<u8> for AsciiSym {
    type Error = VariantError<u8>;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        AsciiChar::from_ascii(value).map_err(|_| VariantError::with::<AsciiSym>(value)).map(Self)
    }
}
impl StrictType for AsciiSym {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_STD;
    fn strict_name() -> Option<TypeName> { Some(tn!("Ascii")) }
}
impl StrictSum for AsciiSym {
    const ALL_VARIANTS: &'static [(u8, &'static str)] = &[
        (b'\0', "nul"),
        (0x01, "soh"),
        (0x02, "stx"),
        (0x03, "etx"),
        (0x04, "eot"),
        (0x05, "enq"),
        (0x06, "ack"),
        (0x07, "bel"),
        (0x08, "bs"),
        (b'\t', "ht"),
        (b'\n', "lf"),
        (0x0b, "vt"),
        (0x0c, "ff"),
        (b'\r', "cr"),
        (0x0e, "so"),
        (0x0f, "si"),
        (0x10, "dle"),
        (0x11, "dc1"),
        (0x12, "dc2"),
        (0x13, "dc3"),
        (0x14, "dc4"),
        (0x15, "nack"),
        (0x16, "syn"),
        (0x17, "etb"),
        (0x18, "can"),
        (0x19, "em"),
        (0x1a, "sub"),
        (0x1b, "esc"),
        (0x1c, "fs"),
        (0x1d, "gs"),
        (0x1e, "rs"),
        (0x1f, "us"),
        (b' ', "space"),
        (b'!', "excl"),
        (b'"', "quotes"),
        (b'#', "hash"),
        (b'$', "dollar"),
        (b'%', "percent"),
        (b'&', "ampersand"),
        (b'\'', "apostrophe"),
        (b'(', "bracketL"),
        (b')', "bracketR"),
        (b'*', "asterisk"),
        (b'+', "plus"),
        (b',', "comma"),
        (b'-', "minus"),
        (b'.', "dot"),
        (b'/', "slash"),
        (b'0', "zero"),
        (b'1', "one"),
        (b'2', "two"),
        (b'3', "three"),
        (b'4', "four"),
        (b'5', "five"),
        (b'6', "six"),
        (b'7', "seven"),
        (b'8', "eight"),
        (b'9', "nine"),
        (b':', "colon"),
        (b';', "semiColon"),
        (b'<', "less"),
        (b'=', "equal"),
        (b'>', "greater"),
        (b'?', "question"),
        (b'@', "at"),
        (b'A', "_A"),
        (b'B', "_B"),
        (b'C', "_C"),
        (b'D', "_D"),
        (b'E', "_E"),
        (b'F', "_F"),
        (b'G', "_G"),
        (b'H', "_H"),
        (b'I', "_I"),
        (b'J', "_J"),
        (b'K', "_K"),
        (b'L', "_L"),
        (b'M', "_M"),
        (b'N', "_N"),
        (b'O', "_O"),
        (b'P', "_P"),
        (b'Q', "_Q"),
        (b'R', "_R"),
        (b'S', "_S"),
        (b'T', "_T"),
        (b'U', "_U"),
        (b'V', "_V"),
        (b'W', "_W"),
        (b'X', "_X"),
        (b'Y', "_Y"),
        (b'Z', "_Z"),
        (b'[', "sqBracketL"),
        (b'\\', "backSlash"),
        (b']', "sqBracketR"),
        (b'^', "caret"),
        (b'_', "lodash"),
        (b'`', "backtick"),
        (b'a', "a"),
        (b'b', "b"),
        (b'c', "c"),
        (b'd', "d"),
        (b'e', "e"),
        (b'f', "f"),
        (b'g', "g"),
        (b'h', "h"),
        (b'i', "i"),
        (b'j', "j"),
        (b'k', "k"),
        (b'l', "l"),
        (b'm', "m"),
        (b'n', "n"),
        (b'o', "o"),
        (b'p', "p"),
        (b'q', "q"),
        (b'r', "r"),
        (b's', "s"),
        (b't', "t"),
        (b'u', "u"),
        (b'v', "v"),
        (b'w', "w"),
        (b'x', "x"),
        (b'y', "y"),
        (b'z', "z"),
        (b'{', "cBracketL"),
        (b'|', "pipe"),
        (b'}', "cBracketR"),
        (b'~', "tilde"),
        (0x7f, "del"),
    ];
    fn variant_name(&self) -> &'static str {
        Self::ALL_VARIANTS
            .iter()
            .find(|(s, _)| *s == self.as_byte())
            .map(|(_, v)| *v)
            .expect("missed ASCII character variant")
    }
}
impl StrictEnum for AsciiSym {}
impl StrictEncode for AsciiSym {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> { writer.write_enum(*self) }
}
impl StrictDecode for AsciiSym {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_enum()
    }
}

#[derive(Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, Debug)]
#[wrapper_mut(DerefMut)]
#[derive(StrictDumb)]
#[strict_type(lib = LIB_NAME_STD, dumb = Self(AsciiChar::A), crate = crate)]
pub struct AsciiPrintable(AsciiChar);

impl From<AsciiPrintable> for u8 {
    fn from(value: AsciiPrintable) -> Self { value.0.as_byte() }
}

impl TryFrom<u8> for AsciiPrintable {
    type Error = VariantError<u8>;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        AsciiChar::from_ascii(value)
            .map_err(|_| VariantError::with::<AsciiPrintable>(value))
            .map(Self)
    }
}

impl StrictType for AsciiPrintable {
    const STRICT_LIB_NAME: &'static str = LIB_NAME_STD;
}

impl StrictSum for AsciiPrintable {
    const ALL_VARIANTS: &'static [(u8, &'static str)] = &[
        (b' ', "space"),
        (b'!', "excl"),
        (b'"', "quotes"),
        (b'#', "hash"),
        (b'$', "dollar"),
        (b'%', "percent"),
        (b'&', "ampersand"),
        (b'\'', "apostrophe"),
        (b'(', "bracketL"),
        (b')', "bracketR"),
        (b'*', "asterisk"),
        (b'+', "plus"),
        (b',', "comma"),
        (b'-', "minus"),
        (b'.', "dot"),
        (b'/', "slash"),
        (b'0', "zero"),
        (b'1', "one"),
        (b'2', "two"),
        (b'3', "three"),
        (b'4', "four"),
        (b'5', "five"),
        (b'6', "six"),
        (b'7', "seven"),
        (b'8', "eight"),
        (b'9', "nine"),
        (b':', "colon"),
        (b';', "semiColon"),
        (b'<', "less"),
        (b'=', "equal"),
        (b'>', "greater"),
        (b'?', "question"),
        (b'@', "at"),
        (b'A', "_A"),
        (b'B', "_B"),
        (b'C', "_C"),
        (b'D', "_D"),
        (b'E', "_E"),
        (b'F', "_F"),
        (b'G', "_G"),
        (b'H', "_H"),
        (b'I', "_I"),
        (b'J', "_J"),
        (b'K', "_K"),
        (b'L', "_L"),
        (b'M', "_M"),
        (b'N', "_N"),
        (b'O', "_O"),
        (b'P', "_P"),
        (b'Q', "_Q"),
        (b'R', "_R"),
        (b'S', "_S"),
        (b'T', "_T"),
        (b'U', "_U"),
        (b'V', "_V"),
        (b'W', "_W"),
        (b'X', "_X"),
        (b'Y', "_Y"),
        (b'Z', "_Z"),
        (b'[', "sqBracketL"),
        (b'\\', "backSlash"),
        (b']', "sqBracketR"),
        (b'^', "caret"),
        (b'_', "lodash"),
        (b'`', "backtick"),
        (b'a', "a"),
        (b'b', "b"),
        (b'c', "c"),
        (b'd', "d"),
        (b'e', "e"),
        (b'f', "f"),
        (b'g', "g"),
        (b'h', "h"),
        (b'i', "i"),
        (b'j', "j"),
        (b'k', "k"),
        (b'l', "l"),
        (b'm', "m"),
        (b'n', "n"),
        (b'o', "o"),
        (b'p', "p"),
        (b'q', "q"),
        (b'r', "r"),
        (b's', "s"),
        (b't', "t"),
        (b'u', "u"),
        (b'v', "v"),
        (b'w', "w"),
        (b'x', "x"),
        (b'y', "y"),
        (b'z', "z"),
        (b'{', "cBracketL"),
        (b'|', "pipe"),
        (b'}', "cBracketR"),
        (b'~', "tilde"),
    ];
    fn variant_name(&self) -> &'static str {
        Self::ALL_VARIANTS
            .iter()
            .find(|(s, _)| *s == self.as_byte())
            .map(|(_, v)| *v)
            .expect("missed ASCII character variant")
    }
}
impl StrictEnum for AsciiPrintable {}
impl StrictEncode for AsciiPrintable {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> { writer.write_enum(*self) }
}
impl StrictDecode for AsciiPrintable {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_enum()
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaCaps {
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaCapsDot {
    #[strict_type(dumb)]
    #[display(".")]
    Dot = b'.',
    #[strict_type(rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaCapsDash {
    #[strict_type(dumb)]
    #[display("-")]
    Dash = b'-',
    #[strict_type(rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaCapsLodash {
    #[strict_type(rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[strict_type(dumb)]
    #[display("_")]
    Lodash = b'_',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum AlphaSmall {
    #[strict_type(dumb)]
    #[display("a")]
    A = b'a',
    #[display("b")]
    B = b'b',
    #[display("c")]
    C = b'c',
    #[display("d")]
    D = b'd',
    #[display("e")]
    E = b'e',
    #[display("f")]
    F = b'f',
    #[display("g")]
    G = b'g',
    #[display("h")]
    H = b'h',
    #[display("i")]
    I = b'i',
    #[display("j")]
    J = b'j',
    #[display("k")]
    K = b'k',
    #[display("l")]
    L = b'l',
    #[display("m")]
    M = b'm',
    #[display("n")]
    N = b'n',
    #[display("o")]
    O = b'o',
    #[display("p")]
    P = b'p',
    #[display("q")]
    Q = b'q',
    #[display("r")]
    R = b'r',
    #[display("s")]
    S = b's',
    #[display("t")]
    T = b't',
    #[display("u")]
    U = b'u',
    #[display("v")]
    V = b'v',
    #[display("w")]
    W = b'w',
    #[display("x")]
    X = b'x',
    #[display("y")]
    Y = b'y',
    #[display("z")]
    Z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum AlphaSmallDot {
    #[strict_type(dumb)]
    #[display(".")]
    Dot = b'.',
    #[display("a")]
    A = b'a',
    #[display("b")]
    B = b'b',
    #[display("c")]
    C = b'c',
    #[display("d")]
    D = b'd',
    #[display("e")]
    E = b'e',
    #[display("f")]
    F = b'f',
    #[display("g")]
    G = b'g',
    #[display("h")]
    H = b'h',
    #[display("i")]
    I = b'i',
    #[display("j")]
    J = b'j',
    #[display("k")]
    K = b'k',
    #[display("l")]
    L = b'l',
    #[display("m")]
    M = b'm',
    #[display("n")]
    N = b'n',
    #[display("o")]
    O = b'o',
    #[display("p")]
    P = b'p',
    #[display("q")]
    Q = b'q',
    #[display("r")]
    R = b'r',
    #[display("s")]
    S = b's',
    #[display("t")]
    T = b't',
    #[display("u")]
    U = b'u',
    #[display("v")]
    V = b'v',
    #[display("w")]
    W = b'w',
    #[display("x")]
    X = b'x',
    #[display("y")]
    Y = b'y',
    #[display("z")]
    Z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum AlphaSmallDash {
    #[strict_type(dumb)]
    #[display("-")]
    Dash = b'-',
    #[display("a")]
    A = b'a',
    #[display("b")]
    B = b'b',
    #[display("c")]
    C = b'c',
    #[display("d")]
    D = b'd',
    #[display("e")]
    E = b'e',
    #[display("f")]
    F = b'f',
    #[display("g")]
    G = b'g',
    #[display("h")]
    H = b'h',
    #[display("i")]
    I = b'i',
    #[display("j")]
    J = b'j',
    #[display("k")]
    K = b'k',
    #[display("l")]
    L = b'l',
    #[display("m")]
    M = b'm',
    #[display("n")]
    N = b'n',
    #[display("o")]
    O = b'o',
    #[display("p")]
    P = b'p',
    #[display("q")]
    Q = b'q',
    #[display("r")]
    R = b'r',
    #[display("s")]
    S = b's',
    #[display("t")]
    T = b't',
    #[display("u")]
    U = b'u',
    #[display("v")]
    V = b'v',
    #[display("w")]
    W = b'w',
    #[display("x")]
    X = b'x',
    #[display("y")]
    Y = b'y',
    #[display("z")]
    Z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum AlphaSmallLodash {
    #[strict_type(dumb)]
    #[display("_")]
    Lodash = b'_',
    #[display("a")]
    A = b'a',
    #[display("b")]
    B = b'b',
    #[display("c")]
    C = b'c',
    #[display("d")]
    D = b'd',
    #[display("e")]
    E = b'e',
    #[display("f")]
    F = b'f',
    #[display("g")]
    G = b'g',
    #[display("h")]
    H = b'h',
    #[display("i")]
    I = b'i',
    #[display("j")]
    J = b'j',
    #[display("k")]
    K = b'k',
    #[display("l")]
    L = b'l',
    #[display("m")]
    M = b'm',
    #[display("n")]
    N = b'n',
    #[display("o")]
    O = b'o',
    #[display("p")]
    P = b'p',
    #[display("q")]
    Q = b'q',
    #[display("r")]
    R = b'r',
    #[display("s")]
    S = b's',
    #[display("t")]
    T = b't',
    #[display("u")]
    U = b'u',
    #[display("v")]
    V = b'v',
    #[display("w")]
    W = b'w',
    #[display("x")]
    X = b'x',
    #[display("y")]
    Y = b'y',
    #[display("z")]
    Z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum Alpha {
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaDot {
    #[strict_type(dumb)]
    #[display(".")]
    Dot = b'.',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaDash {
    #[strict_type(dumb)]
    #[display("-")]
    Dash = b'-',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaLodash {
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[strict_type(dumb)]
    #[display("_")]
    Lodash = b'_',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum Dec {
    #[strict_type(dumb)]
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[repr(u8)]
pub enum DecDot {
    #[strict_type(dumb)]
    #[display(".")]
    Dot = b'.',
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum HexDecCaps {
    #[strict_type(dumb)]
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[display("A")]
    Ten = b'A',
    #[display("B")]
    Eleven = b'B',
    #[display("C")]
    Twelve = b'C',
    #[display("D")]
    Thirteen = b'D',
    #[display("E")]
    Fourteen = b'E',
    #[display("F")]
    Fifteen = b'F',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum HexDecSmall {
    #[strict_type(dumb)]
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[display("a")]
    Ten = b'a',
    #[display("b")]
    Eleven = b'b',
    #[display("c")]
    Twelve = b'c',
    #[display("d")]
    Thirteen = b'd',
    #[display("e")]
    Fourteen = b'e',
    #[display("f")]
    Fifteen = b'f',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaCapsNum {
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaNum {
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaNumDot {
    #[strict_type(dumb)]
    #[display(".")]
    Dot = b'.',
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaNumDash {
    #[strict_type(dumb)]
    #[display("-")]
    Dash = b'-',
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[strict_type(dumb, rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[derive(StrictDumb, StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_STD, tags = repr, into_u8, try_from_u8, crate = crate)]
#[display(inner)]
#[repr(u8)]
pub enum AlphaNumLodash {
    #[display("0")]
    Zero = b'0',
    #[display("1")]
    One = b'1',
    #[display("2")]
    Two = b'2',
    #[display("3")]
    Three = b'3',
    #[display("4")]
    Four = b'4',
    #[display("5")]
    Five = b'5',
    #[display("6")]
    Six = b'6',
    #[display("7")]
    Seven = b'7',
    #[display("8")]
    Eight = b'8',
    #[display("9")]
    Nine = b'9',
    #[strict_type(rename = "_A")]
    A = b'A',
    #[strict_type(rename = "_B")]
    B = b'B',
    #[strict_type(rename = "_C")]
    C = b'C',
    #[strict_type(rename = "_D")]
    D = b'D',
    #[strict_type(rename = "_E")]
    E = b'E',
    #[strict_type(rename = "_F")]
    F = b'F',
    #[strict_type(rename = "_G")]
    G = b'G',
    #[strict_type(rename = "_H")]
    H = b'H',
    #[strict_type(rename = "_I")]
    I = b'I',
    #[strict_type(rename = "_J")]
    J = b'J',
    #[strict_type(rename = "_K")]
    K = b'K',
    #[strict_type(rename = "_L")]
    L = b'L',
    #[strict_type(rename = "_M")]
    M = b'M',
    #[strict_type(rename = "_N")]
    N = b'N',
    #[strict_type(rename = "_O")]
    O = b'O',
    #[strict_type(rename = "_P")]
    P = b'P',
    #[strict_type(rename = "_Q")]
    Q = b'Q',
    #[strict_type(rename = "_R")]
    R = b'R',
    #[strict_type(rename = "_S")]
    S = b'S',
    #[strict_type(rename = "_T")]
    T = b'T',
    #[strict_type(rename = "_U")]
    U = b'U',
    #[strict_type(rename = "_V")]
    V = b'V',
    #[strict_type(rename = "_W")]
    W = b'W',
    #[strict_type(rename = "_X")]
    X = b'X',
    #[strict_type(rename = "_Y")]
    Y = b'Y',
    #[strict_type(rename = "_Z")]
    Z = b'Z',
    #[strict_type(dumb)]
    #[display("_")]
    Lodash = b'_',
    #[display("a")]
    a = b'a',
    #[display("b")]
    b = b'b',
    #[display("c")]
    c = b'c',
    #[display("d")]
    d = b'd',
    #[display("e")]
    e = b'e',
    #[display("f")]
    f = b'f',
    #[display("g")]
    g = b'g',
    #[display("h")]
    h = b'h',
    #[display("i")]
    i = b'i',
    #[display("j")]
    j = b'j',
    #[display("k")]
    k = b'k',
    #[display("l")]
    l = b'l',
    #[display("m")]
    m = b'm',
    #[display("n")]
    n = b'n',
    #[display("o")]
    o = b'o',
    #[display("p")]
    p = b'p',
    #[display("q")]
    q = b'q',
    #[display("r")]
    r = b'r',
    #[display("s")]
    s = b's',
    #[display("t")]
    t = b't',
    #[display("u")]
    u = b'u',
    #[display("v")]
    v = b'v',
    #[display("w")]
    w = b'w',
    #[display("x")]
    x = b'x',
    #[display("y")]
    y = b'y',
    #[display("z")]
    z = b'z',
}

impl RestrictedCharSet for AsciiPrintable {}
impl RestrictedCharSet for AsciiSym {}
impl RestrictedCharSet for Alpha {}
impl RestrictedCharSet for AlphaDot {}
impl RestrictedCharSet for AlphaDash {}
impl RestrictedCharSet for AlphaLodash {}
impl RestrictedCharSet for AlphaCaps {}
impl RestrictedCharSet for AlphaCapsDot {}
impl RestrictedCharSet for AlphaCapsDash {}
impl RestrictedCharSet for AlphaCapsLodash {}
impl RestrictedCharSet for AlphaSmall {}
impl RestrictedCharSet for AlphaSmallDot {}
impl RestrictedCharSet for AlphaSmallDash {}
impl RestrictedCharSet for AlphaSmallLodash {}
impl RestrictedCharSet for AlphaNum {}
impl RestrictedCharSet for AlphaNumDot {}
impl RestrictedCharSet for AlphaNumDash {}
impl RestrictedCharSet for AlphaNumLodash {}
impl RestrictedCharSet for AlphaCapsNum {}
impl RestrictedCharSet for Dec {}
impl RestrictedCharSet for DecDot {}
impl RestrictedCharSet for HexDecCaps {}
impl RestrictedCharSet for HexDecSmall {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn rstring_utf8() {
        let s = "Юникод";
        assert_eq!(
            RString::<AlphaCaps, Alpha, 1, 8>::from_str(s).unwrap_err(),
            InvalidRString::DisallowedFirst(s.to_owned(), 'Ю')
        );

        let s = "Uникод";
        assert_eq!(
            RString::<AlphaCaps, Alpha, 1, 8>::from_str(s).unwrap_err(),
            InvalidRString::InvalidChar(s.to_owned(), 'н', 1)
        );
    }
}
