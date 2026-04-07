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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::Hash;
use std::io;
use std::num::{NonZeroU128, NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU8};

use amplify::ascii::AsciiString;
use amplify::confinement::Confined;
#[cfg(feature = "float")]
use amplify::num::apfloat::{ieee, Float};
use amplify::num::{i1024, i256, i512, u1024, u24, u256, u40, u48, u512, u56};
use amplify::{Array, Wrapper};

use crate::stl::AsciiSym;
use crate::{
    DecodeError, DefaultBasedStrictDumb, DefineUnion, Primitive, RString, ReadRaw, ReadTuple,
    ReadUnion, RestrictedCharSet, Sizing, StrictDecode, StrictDumb, StrictEncode, StrictProduct,
    StrictStruct, StrictSum, StrictTuple, StrictType, StrictUnion, TypeName, TypedRead, TypedWrite,
    WriteRaw, WriteTuple, WriteUnion, LIB_EMBEDDED,
};

pub trait DecodeRawLe: Sized {
    fn decode_raw_le(reader: &mut (impl ReadRaw + ?Sized)) -> Result<Self, DecodeError>;
}

#[derive(
    Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default, From
)]
#[wrapper(Display, FromStr, Octal, BitOps)]
#[wrapper_mut(BitAssign)]
#[derive(StrictType, StrictDecode)]
#[strict_type(lib = LIB_EMBEDDED, crate = crate)]
pub struct Byte(u8);

impl DefaultBasedStrictDumb for Byte {}

impl StrictEncode for Byte {
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        unsafe {
            writer = writer.register_primitive(Primitive::BYTE);
            writer.raw_writer().write_raw::<1>([self.0])?;
        }
        Ok(writer)
    }
}

macro_rules! encode_num {
    ($ty:ty, $id:ident) => {
        impl $crate::StrictType for $ty {
            const STRICT_LIB_NAME: &'static str = $crate::LIB_EMBEDDED;
            fn strict_name() -> Option<TypeName> { Some(tn!(stringify!($id))) }
        }
        impl $crate::StrictEncode for $ty {
            fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
                unsafe {
                    writer = writer.register_primitive(Primitive::$id);
                    writer.raw_writer().write_raw_array(self.to_le_bytes())?;
                }
                Ok(writer)
            }
        }
        impl $crate::DecodeRawLe for $ty {
            fn decode_raw_le(reader: &mut (impl ReadRaw + ?Sized)) -> Result<Self, DecodeError> {
                let buf = reader.read_raw_array::<{ Self::BITS as usize / 8 }>()?;
                Ok(Self::from_le_bytes(buf))
            }
        }
        impl $crate::StrictDecode for $ty {
            fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
                Self::decode_raw_le(unsafe { reader.raw_reader() })
            }
        }
    };
}

macro_rules! encode_nonzero {
    ($ty:ty, $p:ty, $id:ident) => {
        impl $crate::StrictType for $ty {
            const STRICT_LIB_NAME: &'static str = $crate::LIB_EMBEDDED;
            fn strict_name() -> Option<TypeName> { Some(tn!(stringify!($id))) }
        }
        impl $crate::StrictEncode for $ty {
            fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
                unsafe {
                    writer = writer.register_primitive(Primitive::$id);
                    writer.raw_writer().write_raw_array(self.get().to_le_bytes())?;
                }
                Ok(writer)
            }
        }
        impl $crate::StrictDecode for $ty {
            fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
                let buf =
                    unsafe { reader.raw_reader().read_raw_array::<{ Self::BITS as usize / 8 }>()? };
                let v = <$p>::from_le_bytes(buf);
                Self::new(v).ok_or(DecodeError::ZeroNatural)
            }
        }
    };
}

macro_rules! encode_float {
    ($ty:ty, $len:literal, $id:ident) => {
        #[cfg(feature = "float")]
        impl $crate::StrictType for $ty {
            const STRICT_LIB_NAME: &'static str = $crate::LIB_EMBEDDED;
            fn strict_name() -> Option<TypeName> { Some(tn!(stringify!($id))) }
        }
        #[cfg(feature = "float")]
        impl $crate::StrictEncode for $ty {
            fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
                let mut be = [0u8; $len];
                be.copy_from_slice(&self.to_bits().to_le_bytes()[..$len]);
                unsafe {
                    writer = writer.register_primitive(Primitive::$id);
                    writer.raw_writer().write_raw_array(be)?;
                }
                Ok(writer)
            }
        }
        #[cfg(feature = "float")]
        impl $crate::StrictDecode for $ty {
            fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
                const BYTES: usize = <$ty>::BITS / 8;
                let mut inner = [0u8; 32];
                let buf = unsafe { reader.raw_reader().read_raw_array::<BYTES>()? };
                inner[..BYTES].copy_from_slice(&buf[..]);
                let bits = u256::from_le_bytes(inner);
                Ok(Self::from_bits(bits))
            }
        }
    };
}

encode_num!(u8, U8);
encode_num!(u16, U16);
encode_num!(u24, U24);
encode_num!(u32, U32);
encode_num!(u40, U40);
encode_num!(u48, U48);
encode_num!(u56, U56);
encode_num!(u64, U64);
encode_num!(u128, U128);
encode_num!(u256, U256);
encode_num!(u512, U512);
encode_num!(u1024, U1024);

encode_num!(i8, I8);
encode_num!(i16, I16);
encode_num!(i32, I32);
encode_num!(i64, I64);
encode_num!(i128, I128);
encode_num!(i256, I256);
encode_num!(i512, I512);
encode_num!(i1024, I1024);

encode_nonzero!(NonZeroU8, u8, N8);
encode_nonzero!(NonZeroU16, u16, N16);
encode_nonzero!(NonZeroU32, u32, U32);
encode_nonzero!(NonZeroU64, u64, U64);
encode_nonzero!(NonZeroU128, u128, U128);

encode_float!(ieee::Half, 2, F16);
encode_float!(ieee::Single, 4, F32);
encode_float!(ieee::Double, 8, F64);
encode_float!(ieee::X87DoubleExtended, 10, F80);
encode_float!(ieee::Quad, 16, F128);
encode_float!(ieee::Oct, 32, F256);

impl<T> StrictType for Box<T>
where T: StrictType
{
    const STRICT_LIB_NAME: &'static str = T::STRICT_LIB_NAME;
}
impl<T> StrictSum for Box<T>
where T: StrictSum
{
    const ALL_VARIANTS: &'static [(u8, &'static str)] = T::ALL_VARIANTS;
    fn variant_name(&self) -> &'static str { self.as_ref().variant_name() }
}
impl<T> DefaultBasedStrictDumb for Box<T> {}
impl<T> StrictProduct for Box<T> where T: Default + StrictProduct {}
impl<T> StrictUnion for Box<T> where T: Default + StrictUnion {}
impl<T> StrictTuple for Box<T>
where T: Default + StrictTuple
{
    const FIELD_COUNT: u8 = T::FIELD_COUNT;
}
impl<T> StrictStruct for Box<T>
where T: Default + StrictStruct
{
    const ALL_FIELDS: &'static [&'static str] = T::ALL_FIELDS;
}
impl<T> StrictEncode for Box<T>
where T: StrictEncode
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        self.as_ref().strict_encode(writer)
    }
}
impl<T> StrictDecode for Box<T>
where T: StrictDecode
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        T::strict_decode(reader).map(Box::new)
    }
}

impl<T> StrictType for Option<T>
where T: StrictType
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T> StrictSum for Option<T>
where T: StrictType
{
    const ALL_VARIANTS: &'static [(u8, &'static str)] = &[(0u8, "none"), (1u8, "some")];
    fn variant_name(&self) -> &'static str {
        match self {
            None => "none",
            Some(_) => "some",
        }
    }
}
impl<T> StrictUnion for Option<T> where T: StrictType {}
impl<T: StrictEncode + StrictDumb> StrictEncode for Option<T> {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_union::<Self>(|u| {
            let u = u.define_unit(vname!("none")).define_newtype::<T>(vname!("some")).complete();

            Ok(match self {
                None => u.write_unit(vname!("none")),
                Some(val) => u.write_newtype(vname!("some"), val),
            }?
            .complete())
        })
    }
}
impl<T: StrictDecode> StrictDecode for Option<T> {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_union(|field_name, u| match field_name.as_str() {
            "none" => Ok(None),
            "some" => u.read_tuple(|r| r.read_field().map(Some)),
            _ => unreachable!("unknown option field"),
        })
    }
}

impl StrictType for () {
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl StrictEncode for () {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        Ok(unsafe { writer.register_primitive(Primitive::UNIT) })
    }
}
impl StrictDecode for () {
    fn strict_decode(_reader: &mut impl TypedRead) -> Result<Self, DecodeError> { Ok(()) }
}

impl<A: StrictType, B: StrictType> StrictType for (A, B) {
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<A: StrictType + Default, B: StrictType + Default> DefaultBasedStrictDumb for (A, B) {}
impl<A: StrictType + Default, B: StrictType + Default> StrictProduct for (A, B) {}
impl<A: StrictType + Default, B: StrictType + Default> StrictTuple for (A, B) {
    const FIELD_COUNT: u8 = 2;
}
impl<A: StrictEncode + Default, B: StrictEncode + Default> StrictEncode for (A, B) {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_tuple::<Self>(|w| Ok(w.write_field(&self.0)?.write_field(&self.1)?.complete()))
    }
}
impl<A: StrictDecode + Default, B: StrictDecode + Default> StrictDecode for (A, B) {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let a = r.read_field()?;
            let b = r.read_field()?;
            Ok((a, b))
        })
    }
}

impl<A: StrictType, B: StrictType, C: StrictType> StrictType for (A, B, C) {
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<A: StrictType + Default, B: StrictType + Default, C: StrictType + Default>
    DefaultBasedStrictDumb for (A, B, C)
{
}
impl<A: StrictType + Default, B: StrictType + Default, C: StrictType + Default> StrictProduct
    for (A, B, C)
{
}
impl<A: StrictType + Default, B: StrictType + Default, C: StrictType + Default> StrictTuple
    for (A, B, C)
{
    const FIELD_COUNT: u8 = 3;
}
impl<A: StrictEncode + Default, B: StrictEncode + Default, C: StrictEncode + Default> StrictEncode
    for (A, B, C)
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        writer.write_tuple::<Self>(|w| {
            Ok(w.write_field(&self.0)?.write_field(&self.1)?.write_field(&self.2)?.complete())
        })
    }
}
impl<A: StrictDecode + Default, B: StrictDecode + Default, C: StrictDecode + Default> StrictDecode
    for (A, B, C)
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        reader.read_tuple(|r| {
            let a = r.read_field()?;
            let b = r.read_field()?;
            let c = r.read_field()?;
            Ok((a, b, c))
        })
    }
}

impl<T: StrictType + Copy + StrictDumb, const LEN: usize> StrictType for [T; LEN] {
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T: StrictEncode + Copy + StrictDumb, const LEN: usize> StrictEncode for [T; LEN] {
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        for item in self {
            writer = item.strict_encode(writer)?;
        }
        Ok(unsafe {
            if T::strict_name() == u8::strict_name() {
                writer.register_array(&Byte::strict_dumb(), LEN as u16)
            } else {
                writer.register_array(&T::strict_dumb(), LEN as u16)
            }
        })
    }
}
impl<T: StrictDecode + Copy + StrictDumb, const LEN: usize> StrictDecode for [T; LEN] {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let mut ar = [T::strict_dumb(); LEN];
        for c in ar.iter_mut() {
            *c = T::strict_decode(reader)?;
        }
        Ok(ar)
    }
}

impl<T: StrictType + StrictDumb + Copy, const LEN: usize, const REVERSE_STR: bool> StrictType
    for Array<T, LEN, REVERSE_STR>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T: StrictEncode + StrictDumb + Copy, const LEN: usize, const REVERSE_STR: bool> StrictEncode
    for Array<T, LEN, REVERSE_STR>
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        self.as_inner().strict_encode(writer)
    }
}
impl<T: StrictDecode + StrictDumb + Copy, const LEN: usize, const REVERSE_STR: bool> StrictDecode
    for Array<T, LEN, REVERSE_STR>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        <[T; LEN]>::strict_decode(reader).map(Self::from_inner)
    }
}

impl<const MIN_LEN: usize, const MAX_LEN: usize> DefaultBasedStrictDumb
    for Confined<String, MIN_LEN, MAX_LEN>
{
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictType for Confined<String, MIN_LEN, MAX_LEN> {
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictEncode
    for Confined<String, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        unsafe {
            writer
                .register_unicode(Sizing::new(MIN_LEN as u64, MAX_LEN as u64))
                .write_string::<MAX_LEN>(self.as_bytes())
        }
    }
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictDecode
    for Confined<String, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let bytes = unsafe { reader.read_string::<MAX_LEN>()? };
        let s = String::from_utf8(bytes)?;
        Confined::try_from(s).map_err(DecodeError::from)
    }
}

impl<const MIN_LEN: usize, const MAX_LEN: usize> DefaultBasedStrictDumb
    for Confined<AsciiString, MIN_LEN, MAX_LEN>
{
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictType
    for Confined<AsciiString, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictEncode
    for Confined<AsciiString, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        unsafe {
            writer
                .register_list(
                    &AsciiSym::strict_dumb(),
                    Sizing::new(MIN_LEN as u64, MAX_LEN as u64),
                )
                .write_string::<MAX_LEN>(self.as_bytes())
        }
    }
}
impl<const MIN_LEN: usize, const MAX_LEN: usize> StrictDecode
    for Confined<AsciiString, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let bytes = unsafe { reader.read_string::<MAX_LEN>()? };
        let s = AsciiString::from_ascii(bytes).map_err(|err| err.ascii_error())?;
        Confined::try_from(s).map_err(DecodeError::from)
    }
}

impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN_LEN: usize, const MAX_LEN: usize>
    StrictType for RString<C1, C, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN_LEN: usize, const MAX_LEN: usize>
    StrictDumb for RString<C1, C, MIN_LEN, MAX_LEN>
{
    fn strict_dumb() -> Self {
        Self::try_from(format!(
            "{}{}",
            C1::strict_dumb(),
            String::from_utf8(vec![C::strict_dumb().into(); MIN_LEN - 1]).expect("dumb")
        ))
        .expect("dumb")
    }
}
impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN_LEN: usize, const MAX_LEN: usize>
    StrictEncode for RString<C1, C, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        debug_assert_ne!(
            MIN_LEN, 0,
            "Restricted string type can't have minimum length equal to zero"
        );
        let sizing = Sizing::new(MIN_LEN as u64, MAX_LEN as u64);
        unsafe {
            writer
                .register_rstring(&C::strict_dumb(), &C1::strict_dumb(), sizing)
                .write_string::<MAX_LEN>(self.as_bytes())
        }
    }
}
impl<C1: RestrictedCharSet, C: RestrictedCharSet, const MIN_LEN: usize, const MAX_LEN: usize>
    StrictDecode for RString<C1, C, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let bytes = unsafe { reader.read_string::<MAX_LEN>()? };
        RString::try_from(bytes).map_err(|e| DecodeError::DataIntegrityError(e.to_string()))
    }
}

impl<T: StrictType, const MIN_LEN: usize, const MAX_LEN: usize> StrictType
    for Confined<Vec<T>, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T: StrictEncode + StrictDumb, const MIN_LEN: usize, const MAX_LEN: usize> StrictEncode
    for Confined<Vec<T>, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        let sizing = Sizing::new(MIN_LEN as u64, MAX_LEN as u64);
        writer = unsafe {
            writer = writer.write_collection::<Vec<T>, MIN_LEN, MAX_LEN>(self)?;
            if T::strict_name() == u8::strict_name() {
                writer.register_list(&Byte::strict_dumb(), sizing)
            } else {
                writer.register_list(&T::strict_dumb(), sizing)
            }
        };
        Ok(writer)
    }
}
impl<T: StrictDecode, const MIN_LEN: usize, const MAX_LEN: usize> StrictDecode
    for Confined<Vec<T>, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let len = unsafe { reader.raw_reader().read_raw_len::<MAX_LEN>()? };
        let mut col = Vec::<T>::with_capacity(len);
        for _ in 0..len {
            col.push(StrictDecode::strict_decode(reader)?);
        }
        Confined::try_from(col).map_err(DecodeError::from)
    }
}

impl<T: StrictType, const MIN_LEN: usize, const MAX_LEN: usize> DefaultBasedStrictDumb
    for Confined<VecDeque<T>, MIN_LEN, MAX_LEN>
{
}
impl<T: StrictType, const MIN_LEN: usize, const MAX_LEN: usize> StrictType
    for Confined<VecDeque<T>, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T: StrictEncode + StrictDumb, const MIN_LEN: usize, const MAX_LEN: usize> StrictEncode
    for Confined<VecDeque<T>, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        let sizing = Sizing::new(MIN_LEN as u64, MAX_LEN as u64);
        writer = unsafe {
            writer = writer.write_collection::<VecDeque<T>, MIN_LEN, MAX_LEN>(self)?;
            if T::strict_name() == u8::strict_name() {
                writer.register_list(&Byte::strict_dumb(), sizing)
            } else {
                writer.register_list(&T::strict_dumb(), sizing)
            }
        };
        Ok(writer)
    }
}
impl<T: StrictDecode, const MIN_LEN: usize, const MAX_LEN: usize> StrictDecode
    for Confined<VecDeque<T>, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let len = unsafe { reader.raw_reader().read_raw_len::<MAX_LEN>()? };
        let mut col = VecDeque::<T>::with_capacity(len);
        for _ in 0..len {
            col.push_back(StrictDecode::strict_decode(reader)?);
        }
        Confined::try_from(col).map_err(DecodeError::from)
    }
}

impl<T: StrictType + Ord, const MIN_LEN: usize, const MAX_LEN: usize> DefaultBasedStrictDumb
    for Confined<BTreeSet<T>, MIN_LEN, MAX_LEN>
{
}
impl<T: StrictType + Ord, const MIN_LEN: usize, const MAX_LEN: usize> StrictType
    for Confined<BTreeSet<T>, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<T: StrictEncode + Ord + StrictDumb, const MIN_LEN: usize, const MAX_LEN: usize> StrictEncode
    for Confined<BTreeSet<T>, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        unsafe {
            writer = writer.write_collection::<BTreeSet<T>, MIN_LEN, MAX_LEN>(self)?;
        }
        Ok(unsafe {
            writer.register_set(&T::strict_dumb(), Sizing::new(MIN_LEN as u64, MAX_LEN as u64))
        })
    }
}
impl<T: StrictDecode + Ord, const MIN_LEN: usize, const MAX_LEN: usize> StrictDecode
    for Confined<BTreeSet<T>, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let len = unsafe { reader.raw_reader().read_raw_len::<MAX_LEN>()? };
        let mut col = BTreeSet::<T>::new();
        for _ in 0..len {
            let item = StrictDecode::strict_decode(reader)?;
            if matches!(col.last(), Some(last) if last > &item) {
                return Err(DecodeError::BrokenSetOrder);
            }
            if !col.insert(item) {
                return Err(DecodeError::RepeatedSetValue);
            }
        }
        Confined::try_from(col).map_err(DecodeError::from)
    }
}

impl<K: StrictType + Ord + Hash, V: StrictType, const MIN_LEN: usize, const MAX_LEN: usize>
    DefaultBasedStrictDumb for Confined<BTreeMap<K, V>, MIN_LEN, MAX_LEN>
{
}
impl<K: StrictType + Ord + Hash, V: StrictType, const MIN_LEN: usize, const MAX_LEN: usize>
    StrictType for Confined<BTreeMap<K, V>, MIN_LEN, MAX_LEN>
{
    const STRICT_LIB_NAME: &'static str = LIB_EMBEDDED;
    fn strict_name() -> Option<TypeName> { None }
}
impl<
        K: StrictEncode + Ord + Hash + StrictDumb,
        V: StrictEncode + StrictDumb,
        const MIN_LEN: usize,
        const MAX_LEN: usize,
    > StrictEncode for Confined<BTreeMap<K, V>, MIN_LEN, MAX_LEN>
{
    fn strict_encode<W: TypedWrite>(&self, mut writer: W) -> io::Result<W> {
        unsafe {
            writer.raw_writer().write_raw_len::<MAX_LEN>(self.len())?;
        }
        for (k, v) in self {
            writer = k.strict_encode(writer)?;
            writer = v.strict_encode(writer)?
        }
        Ok(unsafe {
            writer.register_map(
                &K::strict_dumb(),
                &V::strict_dumb(),
                Sizing::new(MIN_LEN as u64, MAX_LEN as u64),
            )
        })
    }
}
impl<
        K: StrictDecode + Ord + Hash + StrictDumb,
        V: StrictDecode + StrictDumb,
        const MIN_LEN: usize,
        const MAX_LEN: usize,
    > StrictDecode for Confined<BTreeMap<K, V>, MIN_LEN, MAX_LEN>
{
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError> {
        let len = unsafe { reader.raw_reader().read_raw_len::<MAX_LEN>()? };
        let mut col = BTreeMap::new();
        for _ in 0..len {
            let key = StrictDecode::strict_decode(reader)?;
            let val = StrictDecode::strict_decode(reader)?;
            if matches!(col.last_key_value(), Some((last, _)) if last > &key) {
                return Err(DecodeError::BrokenMapOrder);
            }
            if col.insert(key, val).is_some() {
                return Err(DecodeError::RepeatedMapValue);
            }
        }
        Confined::try_from(col).map_err(DecodeError::from)
    }
}
