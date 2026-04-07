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

use std::fmt::{self, Display, Formatter, Write};
use std::hash::Hash;

use amplify::Wrapper;

use crate::STRICT_TYPES_LIB;

#[derive(Wrapper, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, From)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde_crate"))]
pub struct Primitive(u8);

impl Primitive {
    pub const U8: Primitive = Primitive::unsigned(1);
    pub const U16: Primitive = Primitive::unsigned(2);
    pub const U24: Primitive = Primitive::unsigned(3);
    pub const U32: Primitive = Primitive::unsigned(4);
    pub const U40: Primitive = Primitive::unsigned(5);
    pub const U48: Primitive = Primitive::unsigned(6);
    pub const U56: Primitive = Primitive::unsigned(7);
    pub const U64: Primitive = Primitive::unsigned(8);
    pub const U128: Primitive = Primitive::unsigned(16);
    pub const U160: Primitive = Primitive::unsigned(20);
    pub const U256: Primitive = Primitive::unsigned(32);
    pub const U512: Primitive = Primitive::unsigned(64);
    pub const U1024: Primitive = Primitive::unsigned(128);

    pub const I8: Primitive = Primitive::signed(1);
    pub const I16: Primitive = Primitive::signed(2);
    pub const I24: Primitive = Primitive::signed(3);
    pub const I32: Primitive = Primitive::signed(4);
    pub const I40: Primitive = Primitive::signed(5);
    pub const I48: Primitive = Primitive::signed(6);
    pub const I56: Primitive = Primitive::signed(7);
    pub const I64: Primitive = Primitive::signed(8);
    pub const I128: Primitive = Primitive::signed(16);
    pub const I256: Primitive = Primitive::signed(32);
    pub const I512: Primitive = Primitive::signed(64);
    pub const I1024: Primitive = Primitive::signed(128);

    pub const N8: Primitive = Primitive::non_zero(1);
    pub const N16: Primitive = Primitive::non_zero(2);
    pub const N24: Primitive = Primitive::non_zero(3);
    pub const N32: Primitive = Primitive::non_zero(4);
    pub const N48: Primitive = Primitive::non_zero(6);
    pub const N64: Primitive = Primitive::non_zero(8);
    pub const N128: Primitive = Primitive::non_zero(16);

    pub const F16: Primitive = Primitive::float(2);
    pub const F32: Primitive = Primitive::float(4);
    pub const F64: Primitive = Primitive::float(8);
    pub const F80: Primitive = Primitive::float(10);
    pub const F128: Primitive = Primitive::float(16);
    pub const F256: Primitive = Primitive::float(32);

    pub const UNIT: Primitive = Primitive(0x00);
    pub const BYTE: Primitive = Primitive(0x40);
    pub const RESERVED: Primitive = Primitive(0x80);
    pub const F16B: Primitive = Primitive(0xC0);

    pub const FLOAT_RESERVED_1: Primitive = Primitive(0xC1);
    pub const FLOAT_RESERVED_2: Primitive = Primitive(0xC3);
    pub const FLOAT_RESERVED_3: Primitive = Primitive(0xC5);
    pub const FLOAT_RESERVED_4: Primitive = Primitive(0xC6);
    pub const FLOAT_RESERVED_5: Primitive = Primitive(0xC7);
    pub const FLOAT_RESERVED_6: Primitive = Primitive(0xC9);
    pub const FLOAT_RESERVED_7: Primitive = Primitive(0xCB);
    pub const FLOAT_RESERVED_8: Primitive = Primitive(0xCC);
    pub const FLOAT_RESERVED_9: Primitive = Primitive(0xCD);
    pub const FLOAT_RESERVED_10: Primitive = Primitive(0xCE);
    pub const FLOAT_RESERVED_11: Primitive = Primitive(0xCF);
    pub const FLOAT_RESERVED_12: Primitive = Primitive(0xD1);
    pub const FLOAT_RESERVED_13: Primitive = Primitive(0xD2);
    pub const FLOAT_RESERVED_14: Primitive = Primitive(0xD3);
    pub const FLOAT_RESERVED_15: Primitive = Primitive(0xD4);
    pub const FLOAT_RESERVED_16: Primitive = Primitive(0xD5);
    pub const FLOAT_RESERVED_17: Primitive = Primitive(0xD6);
    pub const FLOAT_RESERVED_18: Primitive = Primitive(0xD7);
    pub const FLOAT_RESERVED_19: Primitive = Primitive(0xD8);
    pub const FLOAT_RESERVED_20: Primitive = Primitive(0xD9);
    pub const FLOAT_RESERVED_21: Primitive = Primitive(0xDA);
    pub const FLOAT_RESERVED_22: Primitive = Primitive(0xDB);
    pub const FLOAT_RESERVED_23: Primitive = Primitive(0xDC);
    pub const FLOAT_RESERVED_24: Primitive = Primitive(0xDE);
    pub const FLOAT_RESERVED_25: Primitive = Primitive(0xDF);

    pub const FLOAT_RESERVED_26: Primitive = Primitive(0xE1);
    pub const FLOAT_RESERVED_27: Primitive = Primitive(0xE2);
    pub const FLOAT_RESERVED_28: Primitive = Primitive(0xE3);
    pub const FLOAT_RESERVED_29: Primitive = Primitive(0xE4);
    pub const FLOAT_RESERVED_30: Primitive = Primitive(0xE5);
    pub const FLOAT_RESERVED_31: Primitive = Primitive(0xE6);
    pub const FLOAT_RESERVED_32: Primitive = Primitive(0xE7);
    pub const FLOAT_RESERVED_33: Primitive = Primitive(0xE8);
    pub const FLOAT_RESERVED_34: Primitive = Primitive(0xE9);
    pub const FLOAT_RESERVED_35: Primitive = Primitive(0xEA);
    pub const FLOAT_RESERVED_36: Primitive = Primitive(0xEB);
    pub const FLOAT_RESERVED_37: Primitive = Primitive(0xEC);
    pub const FLOAT_RESERVED_38: Primitive = Primitive(0xEE);
    pub const FLOAT_RESERVED_39: Primitive = Primitive(0xEF);

    pub const FLOAT_RESERVED_40: Primitive = Primitive(0xF0);
    pub const FLOAT_RESERVED_41: Primitive = Primitive(0xF1);
    pub const FLOAT_RESERVED_42: Primitive = Primitive(0xF2);
    pub const FLOAT_RESERVED_43: Primitive = Primitive(0xF3);
    pub const FLOAT_RESERVED_44: Primitive = Primitive(0xF4);
    pub const FLOAT_RESERVED_45: Primitive = Primitive(0xF5);
    pub const FLOAT_RESERVED_46: Primitive = Primitive(0xF6);
    pub const FLOAT_RESERVED_47: Primitive = Primitive(0xF7);
    pub const FLOAT_RESERVED_48: Primitive = Primitive(0xF8);
    pub const FLOAT_RESERVED_49: Primitive = Primitive(0xF9);
    pub const FLOAT_RESERVED_50: Primitive = Primitive(0xFA);
    pub const FLOAT_RESERVED_51: Primitive = Primitive(0xFB);
    pub const FLOAT_RESERVED_52: Primitive = Primitive(0xFC);
    pub const FLOAT_RESERVED_53: Primitive = Primitive(0xFE);
    pub const FLOAT_RESERVED_54: Primitive = Primitive(0xFF);

    pub const fn unsigned(bytes: u16) -> Self {
        Primitive(
            NumInfo {
                ty: NumCls::Unsigned,
                size: NumSize::from_bytes(bytes),
            }
            .into_code(),
        )
    }

    pub const fn signed(bytes: u16) -> Self {
        Primitive(
            NumInfo {
                ty: NumCls::Signed,
                size: NumSize::from_bytes(bytes),
            }
            .into_code(),
        )
    }

    pub const fn non_zero(bytes: u16) -> Self {
        Primitive(
            NumInfo {
                ty: NumCls::NonZero,
                size: NumSize::from_bytes(bytes),
            }
            .into_code(),
        )
    }

    pub const fn float(bytes: u16) -> Self {
        Primitive(
            NumInfo {
                ty: NumCls::Float,
                size: NumSize::from_bytes(bytes),
            }
            .into_code(),
        )
    }

    pub const fn from_code(code: u8) -> Self { Primitive(code) }
    pub const fn into_code(self) -> u8 { self.0 }

    pub const fn info(self) -> NumInfo { NumInfo::from_code(self.0) }

    pub const fn byte_size(self) -> u16 { self.info().byte_size() }
}

impl Display for Primitive {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Primitive::UNIT => return f.write_str("()"),
            Primitive::BYTE => return f.write_str("Byte"),
            Primitive::F16B => return f.write_str("F16b"),
            Primitive::RESERVED => unreachable!("reserved primitive value"),
            _ => {}
        }

        let info = self.info();
        f.write_char(match info.ty {
            NumCls::Unsigned => 'U',
            NumCls::Signed => 'I',
            NumCls::NonZero => 'N',
            NumCls::Float => 'F',
        })?;

        write!(f, "{}", info.byte_size() * 8)
    }
}

impl_strict_newtype!(Primitive, STRICT_TYPES_LIB);

/// Information about numeric type
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NumInfo {
    /// Class of the number
    pub ty: NumCls,
    /// Size of the number, in bytes
    pub size: NumSize,
}

impl NumInfo {
    pub const fn from_code(id: u8) -> Self {
        NumInfo {
            ty: NumCls::from_code(id),
            size: NumSize::from_code(id),
        }
    }

    pub const fn into_code(self) -> u8 { self.ty.into_code() | self.size.into_code() }

    pub const fn byte_size(self) -> u16 { self.size.byte_size() }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NumSize(NumSizeInner);

impl NumSize {
    pub(super) const fn from_bytes(bytes: u16) -> Self {
        NumSize(if bytes < 0x20 {
            NumSizeInner::Bytes(bytes as u8)
        } else if bytes % 16 != 0 {
            unreachable!()
        } else {
            NumSizeInner::Factored((bytes / 16 - 2) as u8)
        })
    }

    pub(super) const fn from_code(id: u8) -> Self {
        let code = id & 0x1F;
        NumSize(match (id & 0x20) / 0x20 {
            0 if id == Primitive::BYTE.0 => NumSizeInner::Bytes(1),
            0 if id == Primitive::F16B.0 => NumSizeInner::Bytes(2),
            0 => NumSizeInner::Bytes(code),
            1 => NumSizeInner::Factored(code),
            _ => unreachable!(),
        })
    }

    pub(super) const fn into_code(self) -> u8 {
        match self.0 {
            NumSizeInner::Bytes(bytes) => bytes,
            NumSizeInner::Factored(factor) => factor | 0x20,
        }
    }

    pub const fn byte_size(self) -> u16 {
        match self.0 {
            NumSizeInner::Bytes(bytes) => bytes as u16,
            NumSizeInner::Factored(factor) => 16 * (factor as u16 + 2),
        }
    }
}

/// The way how the size is computed and encoded in the type id
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum NumSizeInner {
    /// Lowest 5 bits contain type size in bytes
    Bytes(u8),
    /// Lowest 5 bits contain a factor defining the size according to the
    /// equation `16 * (2 + factor)`
    // TODO: Ensure that U256 doesn't have two encodings with both variants
    Factored(u8),
}

/// Class of the number type
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum NumCls {
    Unsigned = 0x00,
    Signed = 0x40,
    NonZero = 0x80,
    Float = 0xC0,
}

impl NumCls {
    pub const fn from_code(id: u8) -> Self {
        match id & 0xC0 {
            x if x == NumCls::Unsigned as u8 => NumCls::Unsigned,
            x if x == NumCls::Signed as u8 => NumCls::Signed,
            x if x == NumCls::NonZero as u8 => NumCls::NonZero,
            x if x == NumCls::Float as u8 => NumCls::Float,
            _ => unreachable!(),
        }
    }

    pub const fn into_code(self) -> u8 { self as u8 }
}

#[cfg(test)]
mod test {
    use crate::Primitive;

    #[test]
    fn unsigned_byte_size() {
        assert_eq!(Primitive::U8.byte_size(), 1);
        assert_eq!(Primitive::U16.byte_size(), 2);
        assert_eq!(Primitive::U24.byte_size(), 3);
        assert_eq!(Primitive::U32.byte_size(), 4);
        assert_eq!(Primitive::U40.byte_size(), 5);
        assert_eq!(Primitive::U48.byte_size(), 6);
        assert_eq!(Primitive::U56.byte_size(), 7);
        assert_eq!(Primitive::U64.byte_size(), 8);
        assert_eq!(Primitive::U128.byte_size(), 16);
        assert_eq!(Primitive::U160.byte_size(), 20);
        assert_eq!(Primitive::U256.byte_size(), 32);
        assert_eq!(Primitive::U512.byte_size(), 64);
        assert_eq!(Primitive::U1024.byte_size(), 128);
    }

    #[test]
    fn signed_byte_size() {
        assert_eq!(Primitive::I8.byte_size(), 1);
        assert_eq!(Primitive::I16.byte_size(), 2);
        assert_eq!(Primitive::I24.byte_size(), 3);
        assert_eq!(Primitive::I32.byte_size(), 4);
        assert_eq!(Primitive::I40.byte_size(), 5);
        assert_eq!(Primitive::I48.byte_size(), 6);
        assert_eq!(Primitive::I56.byte_size(), 7);
        assert_eq!(Primitive::I64.byte_size(), 8);
        assert_eq!(Primitive::I128.byte_size(), 16);
        assert_eq!(Primitive::I256.byte_size(), 32);
        assert_eq!(Primitive::I512.byte_size(), 64);
        assert_eq!(Primitive::I1024.byte_size(), 128);
    }

    #[test]
    fn nonzero_byte_size() {
        assert_eq!(Primitive::N8.byte_size(), 1);
        assert_eq!(Primitive::N16.byte_size(), 2);
        assert_eq!(Primitive::N24.byte_size(), 3);
        assert_eq!(Primitive::N32.byte_size(), 4);
        assert_eq!(Primitive::N48.byte_size(), 6);
        assert_eq!(Primitive::N64.byte_size(), 8);
        assert_eq!(Primitive::N128.byte_size(), 16);
    }

    #[test]
    fn float_byte_size() {
        assert_eq!(Primitive::F16.byte_size(), 2);
        assert_eq!(Primitive::F16B.byte_size(), 2);
        assert_eq!(Primitive::F32.byte_size(), 4);
        assert_eq!(Primitive::F64.byte_size(), 8);
        assert_eq!(Primitive::F80.byte_size(), 10);
        assert_eq!(Primitive::F128.byte_size(), 16);
        assert_eq!(Primitive::F256.byte_size(), 32);
    }

    #[test]
    fn spec_byte_size() {
        assert_eq!(Primitive::UNIT.byte_size(), 0);
        assert_eq!(Primitive::BYTE.byte_size(), 1);
    }
}
