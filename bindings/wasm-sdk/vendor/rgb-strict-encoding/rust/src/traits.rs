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

use std::io::{BufRead, Seek};
use std::marker::PhantomData;
use std::{fs, io};

use amplify::confinement::{Collection, Confined};
use amplify::num::u24;
use amplify::Wrapper;

use super::{DecodeError, DecodeRawLe, VariantName};
use crate::reader::StreamReader;
use crate::writer::StreamWriter;
use crate::{
    DeserializeError, FieldName, Primitive, SerializeError, Sizing, StrictDumb, StrictEnum,
    StrictReader, StrictStruct, StrictSum, StrictTuple, StrictType, StrictUnion, StrictWriter,
};

pub trait TypedParent: Sized {}

pub trait WriteRaw {
    fn write_raw<const MAX_LEN: usize>(&mut self, bytes: impl AsRef<[u8]>) -> io::Result<()>;
    fn write_raw_array<const LEN: usize>(&mut self, raw: [u8; LEN]) -> io::Result<()> {
        self.write_raw::<LEN>(raw)
    }
    fn write_raw_len<const MAX_LEN: usize>(&mut self, len: usize) -> io::Result<()> {
        match MAX_LEN {
            tiny if tiny <= u8::MAX as usize => self.write_raw_array((len as u8).to_le_bytes()),
            small if small <= u16::MAX as usize => self.write_raw_array((len as u16).to_le_bytes()),
            medium if medium <= u24::MAX.into_usize() => {
                self.write_raw_array((u24::with(len as u32)).to_le_bytes())
            }
            large if large <= u32::MAX as usize => self.write_raw_array((len as u32).to_le_bytes()),
            huge if huge <= u64::MAX as usize => self.write_raw_array((len as u64).to_le_bytes()),
            _ => unreachable!("confined collections larger than u64::MAX must not exist"),
        }
    }
}

impl<T: WriteRaw> WriteRaw for &mut T {
    fn write_raw<const MAX_LEN: usize>(&mut self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        (*self).write_raw::<MAX_LEN>(bytes)
    }
}

#[allow(unused_variables)]
pub trait TypedWrite: Sized {
    type TupleWriter: WriteTuple<Parent = Self>;
    type StructWriter: WriteStruct<Parent = Self>;
    type UnionDefiner: DefineUnion<Parent = Self>;
    type RawWriter: WriteRaw;

    #[doc(hidden)]
    unsafe fn raw_writer(&mut self) -> &mut Self::RawWriter;

    fn write_union<T: StrictUnion>(
        self,
        inner: impl FnOnce(Self::UnionDefiner) -> io::Result<Self>,
    ) -> io::Result<Self>;
    fn write_enum<T: StrictEnum>(self, value: T) -> io::Result<Self>
    where u8: From<T>;
    fn write_tuple<T: StrictTuple>(
        self,
        inner: impl FnOnce(Self::TupleWriter) -> io::Result<Self>,
    ) -> io::Result<Self>;
    fn write_struct<T: StrictStruct>(
        self,
        inner: impl FnOnce(Self::StructWriter) -> io::Result<Self>,
    ) -> io::Result<Self>;
    fn write_newtype<T: StrictTuple>(self, value: &impl StrictEncode) -> io::Result<Self> {
        self.write_tuple::<T>(|writer| Ok(writer.write_field(value)?.complete()))
    }

    #[doc(hidden)]
    unsafe fn register_primitive(self, prim: Primitive) -> Self { self }
    #[doc(hidden)]
    unsafe fn register_array(self, ty: &impl StrictEncode, len: u16) -> Self { self }
    #[doc(hidden)]
    unsafe fn register_unicode(self, sizing: Sizing) -> Self { self }
    #[doc(hidden)]
    #[deprecated(since = "2.3.1", note = "use register_rstring")]
    unsafe fn register_ascii(self, sizing: Sizing) -> Self {
        panic!("TypedWrite::register_ascii must not be called; pls see compilation warnings")
    }
    #[doc(hidden)]
    unsafe fn register_rstring(
        self,
        c: &impl StrictEncode,
        c1: &impl StrictEncode,
        sizing: Sizing,
    ) -> Self {
        self
    }
    #[doc(hidden)]
    unsafe fn register_list(self, ty: &impl StrictEncode, sizing: Sizing) -> Self { self }
    #[doc(hidden)]
    unsafe fn register_set(self, ty: &impl StrictEncode, sizing: Sizing) -> Self { self }
    #[doc(hidden)]
    unsafe fn register_map(
        self,
        ket: &impl StrictEncode,
        ty: &impl StrictEncode,
        sizing: Sizing,
    ) -> Self {
        self
    }

    /// Used by unicode strings, ASCII strings and restricted char set strings.
    #[doc(hidden)]
    unsafe fn write_string<const MAX_LEN: usize>(
        mut self,
        bytes: impl AsRef<[u8]>,
    ) -> io::Result<Self> {
        self.raw_writer().write_raw_len::<MAX_LEN>(bytes.as_ref().len())?;
        self.raw_writer().write_raw::<MAX_LEN>(bytes)?;
        Ok(self)
    }

    /// Vec and sets - excluding strings, written by [`Self::write_string`].
    #[doc(hidden)]
    unsafe fn write_collection<C: Collection, const MIN_LEN: usize, const MAX_LEN: usize>(
        mut self,
        col: &Confined<C, MIN_LEN, MAX_LEN>,
    ) -> io::Result<Self>
    where
        for<'a> &'a C: IntoIterator,
        for<'a> <&'a C as IntoIterator>::Item: StrictEncode,
    {
        self.raw_writer().write_raw_len::<MAX_LEN>(col.len())?;
        for item in col {
            self = item.strict_encode(self)?;
        }
        Ok(self)
    }

    // TODO: Do `write_keyed_collection`
}

pub trait ReadRaw {
    fn read_raw<const MAX_LEN: usize>(&mut self, len: usize) -> io::Result<Vec<u8>>;

    fn read_raw_array<const LEN: usize>(&mut self) -> io::Result<[u8; LEN]>;

    fn read_raw_len<const MAX_LEN: usize>(&mut self) -> Result<usize, DecodeError> {
        Ok(match MAX_LEN {
            tiny if tiny <= u8::MAX as usize => u8::decode_raw_le(self)? as usize,
            small if small <= u16::MAX as usize => u16::decode_raw_le(self)? as usize,
            medium if medium <= u24::MAX.into_usize() => u24::decode_raw_le(self)?.into_usize(),
            large if large <= u32::MAX as usize => u32::decode_raw_le(self)? as usize,
            huge if huge <= u64::MAX as usize => u64::decode_raw_le(self)? as usize,
            _ => unreachable!("confined collections larger than u64::MAX must not exist"),
        })
    }
}

impl<T: ReadRaw> ReadRaw for &mut T {
    fn read_raw<const MAX_LEN: usize>(&mut self, len: usize) -> io::Result<Vec<u8>> {
        (*self).read_raw::<MAX_LEN>(len)
    }

    fn read_raw_array<const LEN: usize>(&mut self) -> io::Result<[u8; LEN]> {
        (*self).read_raw_array::<LEN>()
    }
}

pub trait TypedRead {
    type TupleReader<'parent>: ReadTuple
    where Self: 'parent;
    type StructReader<'parent>: ReadStruct
    where Self: 'parent;
    type UnionReader: ReadUnion;
    type RawReader: ReadRaw;

    #[doc(hidden)]
    unsafe fn raw_reader(&mut self) -> &mut Self::RawReader;

    fn read_union<T: StrictUnion>(
        &mut self,
        inner: impl FnOnce(VariantName, &mut Self::UnionReader) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>;

    fn read_enum<T: StrictEnum>(&mut self) -> Result<T, DecodeError>
    where u8: From<T>;

    fn read_tuple<'parent, 'me, T: StrictTuple>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::TupleReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent;

    fn read_struct<'parent, 'me, T: StrictStruct>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::StructReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent;

    fn read_newtype<T: StrictTuple + Wrapper>(&mut self) -> Result<T, DecodeError>
    where T::Inner: StrictDecode {
        self.read_tuple(|reader| reader.read_field().map(T::from_inner))
    }

    #[doc(hidden)]
    unsafe fn read_string<const MAX_LEN: usize>(&mut self) -> Result<Vec<u8>, DecodeError> {
        let len = self.raw_reader().read_raw_len::<MAX_LEN>()?;
        self.raw_reader().read_raw::<MAX_LEN>(len).map_err(DecodeError::from)
    }
}

pub trait DefineTuple: Sized {
    type Parent: TypedParent;
    fn define_field<T: StrictEncode + StrictDumb>(self) -> Self;
    fn complete(self) -> Self::Parent;
}

pub trait WriteTuple: Sized {
    type Parent: TypedParent;
    fn write_field(self, value: &impl StrictEncode) -> io::Result<Self>;
    fn complete(self) -> Self::Parent;
}

pub trait ReadTuple {
    fn read_field<T: StrictDecode>(&mut self) -> Result<T, DecodeError>;
}

pub trait DefineStruct: Sized {
    type Parent: TypedParent;
    fn define_field<T: StrictEncode + StrictDumb>(self, name: FieldName) -> Self;
    fn complete(self) -> Self::Parent;
}

pub trait WriteStruct: Sized {
    type Parent: TypedParent;
    fn write_field(self, name: FieldName, value: &impl StrictEncode) -> io::Result<Self>;
    fn complete(self) -> Self::Parent;
}

pub trait ReadStruct {
    fn read_field<T: StrictDecode>(&mut self, field: FieldName) -> Result<T, DecodeError>;
}

pub trait DefineEnum: Sized {
    type Parent: TypedWrite;
    type EnumWriter: WriteEnum<Parent = Self::Parent>;
    fn define_variant(self, name: VariantName) -> Self;
    fn complete(self) -> Self::EnumWriter;
}

pub trait WriteEnum: Sized {
    type Parent: TypedWrite;
    fn write_variant(self, name: VariantName) -> io::Result<Self>;
    fn complete(self) -> Self::Parent;
}

pub trait DefineUnion: Sized {
    type Parent: TypedWrite;
    type TupleDefiner: DefineTuple<Parent = Self>;
    type StructDefiner: DefineStruct<Parent = Self>;
    type UnionWriter: WriteUnion<Parent = Self::Parent>;

    fn define_unit(self, name: VariantName) -> Self;
    fn define_newtype<T: StrictEncode + StrictDumb>(self, name: VariantName) -> Self {
        self.define_tuple(name, |definer| definer.define_field::<T>().complete())
    }
    fn define_tuple(
        self,
        name: VariantName,
        inner: impl FnOnce(Self::TupleDefiner) -> Self,
    ) -> Self;
    fn define_struct(
        self,
        name: VariantName,
        inner: impl FnOnce(Self::StructDefiner) -> Self,
    ) -> Self;

    fn complete(self) -> Self::UnionWriter;
}

pub trait WriteUnion: Sized {
    type Parent: TypedWrite;
    type TupleWriter: WriteTuple<Parent = Self>;
    type StructWriter: WriteStruct<Parent = Self>;

    fn write_unit(self, name: VariantName) -> io::Result<Self>;
    fn write_newtype(self, name: VariantName, value: &impl StrictEncode) -> io::Result<Self> {
        self.write_tuple(name, |writer| Ok(writer.write_field(value)?.complete()))
    }
    fn write_tuple(
        self,
        name: VariantName,
        inner: impl FnOnce(Self::TupleWriter) -> io::Result<Self>,
    ) -> io::Result<Self>;
    fn write_struct(
        self,
        name: VariantName,
        inner: impl FnOnce(Self::StructWriter) -> io::Result<Self>,
    ) -> io::Result<Self>;

    fn complete(self) -> Self::Parent;
}

pub trait ReadUnion: Sized {
    type TupleReader<'parent>: ReadTuple
    where Self: 'parent;
    type StructReader<'parent>: ReadStruct
    where Self: 'parent;

    fn read_tuple<'parent, 'me, T: StrictSum>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::TupleReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent;

    fn read_struct<'parent, 'me, T: StrictSum>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::StructReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent;

    fn read_newtype<T: StrictSum + From<I>, I: StrictDecode>(&mut self) -> Result<T, DecodeError> {
        self.read_tuple(|reader| reader.read_field::<I>().map(T::from))
    }
}

pub trait StrictEncode: StrictType {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W>;
    fn strict_write(&self, writer: impl WriteRaw) -> io::Result<()> {
        let w = StrictWriter::with(writer);
        self.strict_encode(w)?;
        Ok(())
    }
}

pub trait StrictDecode: StrictType {
    fn strict_decode(reader: &mut impl TypedRead) -> Result<Self, DecodeError>;
    fn strict_read(reader: impl ReadRaw) -> Result<Self, DecodeError> {
        let mut r = StrictReader::with(reader);
        Self::strict_decode(&mut r)
    }
}

impl<T: StrictEncode> StrictEncode for &T {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> {
        (*self).strict_encode(writer)
    }
}

impl<T> StrictEncode for PhantomData<T> {
    fn strict_encode<W: TypedWrite>(&self, writer: W) -> io::Result<W> { Ok(writer) }
}

impl<T> StrictDecode for PhantomData<T> {
    fn strict_decode(_reader: &mut impl TypedRead) -> Result<Self, DecodeError> { Ok(default!()) }
}

pub trait StrictSerialize: StrictEncode {
    fn strict_serialized_len<const MAX: usize>(&self) -> io::Result<usize> {
        let counter = StrictWriter::counter::<MAX>();
        Ok(self.strict_encode(counter)?.unbox().unconfine().count)
    }

    fn to_strict_serialized<const MAX: usize>(
        &self,
    ) -> Result<Confined<Vec<u8>, 0, MAX>, SerializeError> {
        let ast_data = StrictWriter::in_memory::<MAX>();
        let data = self.strict_encode(ast_data)?.unbox().unconfine();
        Confined::<Vec<u8>, 0, MAX>::try_from(data).map_err(SerializeError::from)
    }

    fn strict_serialize_to_file<const MAX: usize>(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), SerializeError> {
        let file = fs::File::create(path)?;
        // TODO: Do FileReader
        let file = StrictWriter::with(StreamWriter::new::<MAX>(file));
        self.strict_encode(file)?;
        Ok(())
    }
}

pub trait StrictDeserialize: StrictDecode {
    fn from_strict_serialized<const MAX: usize>(
        ast_data: Confined<Vec<u8>, 0, MAX>,
    ) -> Result<Self, DeserializeError> {
        let mut reader = StrictReader::in_memory::<MAX>(ast_data);
        let me = Self::strict_decode(&mut reader)?;
        let mut cursor = reader.into_cursor();
        if !cursor.fill_buf()?.is_empty() {
            return Err(DeserializeError::DataNotEntirelyConsumed);
        }
        Ok(me)
    }

    fn strict_deserialize_from_file<const MAX: usize>(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, DeserializeError> {
        let file = fs::File::open(path)?;
        // TODO: Do FileReader
        let mut reader = StrictReader::with(StreamReader::new::<MAX>(file));
        let me = Self::strict_decode(&mut reader)?;
        let mut file = reader.unbox().unconfine();
        if file.stream_position()? != file.seek(io::SeekFrom::End(0))? {
            return Err(DeserializeError::DataNotEntirelyConsumed);
        }
        Ok(me)
    }
}
