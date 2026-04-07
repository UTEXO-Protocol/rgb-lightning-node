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

use std::io;

use crate::{
    DecodeError, FieldName, ReadRaw, ReadStruct, ReadTuple, ReadUnion, StrictDecode, StrictEnum,
    StrictStruct, StrictSum, StrictTuple, StrictUnion, TypedRead, VariantName,
};

// TODO: Move to amplify crate
/// A simple way to count bytes read through [`io::Read`].
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Default, Debug)]
pub struct ReadCounter {
    /// Count of bytes which passed through this reader
    pub count: usize,
}

impl io::Read for ReadCounter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = buf.len();
        self.count += count;
        Ok(count)
    }
}

// TODO: Move to amplify crate
#[derive(Clone, Debug)]
pub struct ConfinedReader<R: io::Read> {
    count: usize,
    limit: usize,
    reader: R,
}

impl<R: io::Read> From<R> for ConfinedReader<R> {
    fn from(reader: R) -> Self {
        Self {
            count: 0,
            limit: usize::MAX,
            reader,
        }
    }
}

impl<R: io::Read> ConfinedReader<R> {
    pub fn with(limit: usize, reader: R) -> Self {
        Self {
            count: 0,
            limit,
            reader,
        }
    }

    pub fn count(&self) -> usize { self.count }

    pub fn unconfine(self) -> R { self.reader }
}

impl<R: io::Read> io::Read for ConfinedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.reader.read(buf)?;
        match self.count.checked_add(len) {
            None => return Err(io::ErrorKind::OutOfMemory.into()),
            Some(len) if len > self.limit => return Err(io::ErrorKind::InvalidInput.into()),
            Some(len) => self.count = len,
        };
        Ok(len)
    }
}

#[derive(Clone, Debug)]
pub struct StreamReader<R: io::Read>(ConfinedReader<R>);

impl<R: io::Read> StreamReader<R> {
    pub fn new<const MAX: usize>(inner: R) -> Self { Self(ConfinedReader::with(MAX, inner)) }
    pub fn unconfine(self) -> R { self.0.unconfine() }
}

impl<T: AsRef<[u8]>> StreamReader<io::Cursor<T>> {
    pub fn cursor<const MAX: usize>(inner: T) -> Self {
        Self(ConfinedReader::with(MAX, io::Cursor::new(inner)))
    }
}

impl<R: io::Read> ReadRaw for StreamReader<R> {
    fn read_raw<const MAX_LEN: usize>(&mut self, len: usize) -> io::Result<Vec<u8>> {
        use io::Read;
        let mut buf = vec![0u8; len];
        self.0.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_raw_array<const LEN: usize>(&mut self) -> io::Result<[u8; LEN]> {
        use io::Read;
        let mut buf = [0u8; LEN];
        self.0.read_exact(&mut buf)?;
        Ok(buf)
    }
}

impl<T: AsRef<[u8]>> StreamReader<io::Cursor<T>> {
    pub fn in_memory<const MAX: usize>(data: T) -> Self { Self::new::<MAX>(io::Cursor::new(data)) }
    pub fn into_cursor(self) -> io::Cursor<T> { self.0.unconfine() }
}

impl StreamReader<ReadCounter> {
    pub fn counter<const MAX: usize>() -> Self { Self::new::<MAX>(ReadCounter::default()) }
}

#[derive(Clone, Debug, From)]
pub struct StrictReader<R: ReadRaw>(R);

impl<T: AsRef<[u8]>> StrictReader<StreamReader<io::Cursor<T>>> {
    pub fn in_memory<const MAX: usize>(data: T) -> Self {
        Self(StreamReader::in_memory::<MAX>(data))
    }
    pub fn into_cursor(self) -> io::Cursor<T> { self.0.into_cursor() }
}

impl StrictReader<StreamReader<ReadCounter>> {
    pub fn counter<const MAX: usize>() -> Self { Self(StreamReader::counter::<MAX>()) }
}

impl<R: ReadRaw> StrictReader<R> {
    pub fn with(reader: R) -> Self { Self(reader) }

    pub fn unbox(self) -> R { self.0 }
}

impl<R: ReadRaw> TypedRead for StrictReader<R> {
    type TupleReader<'parent>
        = TupleReader<'parent, R>
    where Self: 'parent;
    type StructReader<'parent>
        = StructReader<'parent, R>
    where Self: 'parent;
    type UnionReader = Self;
    type RawReader = R;

    unsafe fn raw_reader(&mut self) -> &mut Self::RawReader { &mut self.0 }

    fn read_union<T: StrictUnion>(
        &mut self,
        inner: impl FnOnce(VariantName, &mut Self::UnionReader) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError> {
        let name = T::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        let tag = u8::strict_decode(self)?;
        let variant_name = T::variant_name_by_tag(tag)
            .ok_or(DecodeError::UnionTagNotKnown(name.to_string(), tag))?;
        inner(variant_name, self)
    }

    fn read_enum<T: StrictEnum>(&mut self) -> Result<T, DecodeError>
    where u8: From<T> {
        let name = T::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        let tag = u8::strict_decode(self)?;
        T::try_from(tag).map_err(|_| DecodeError::EnumTagNotKnown(name.to_string(), tag))
    }

    fn read_tuple<'parent, 'me, T: StrictTuple>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::TupleReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent,
    {
        let name = T::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        let mut reader = TupleReader {
            read_fields: 0,
            parent: self,
        };
        let res = inner(&mut reader)?;
        assert_ne!(reader.read_fields, 0, "you forget to read fields for a tuple {}", name);
        assert_eq!(
            reader.read_fields,
            T::FIELD_COUNT,
            "the number of fields read for a tuple {} doesn't match type declaration",
            name
        );
        Ok(res)
    }

    fn read_struct<'parent, 'me, T: StrictStruct>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::StructReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent,
    {
        let name = T::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        let mut reader = StructReader {
            named_fields: empty!(),
            parent: self,
        };
        let res = inner(&mut reader)?;
        assert!(!reader.named_fields.is_empty(), "you forget to read fields for a tuple {}", name);

        for field in T::ALL_FIELDS {
            let pos = reader
                .named_fields
                .iter()
                .position(|f| f.as_str() == *field)
                .unwrap_or_else(|| panic!("field {} is not read for {}", field, name));
            reader.named_fields.remove(pos);
        }
        assert!(reader.named_fields.is_empty(), "excessive fields are read for {}", name);
        Ok(res)
    }
}

#[derive(Debug)]
pub struct TupleReader<'parent, R: ReadRaw> {
    read_fields: u8,
    parent: &'parent mut StrictReader<R>,
}

impl<R: ReadRaw> ReadTuple for TupleReader<'_, R> {
    fn read_field<T: StrictDecode>(&mut self) -> Result<T, DecodeError> {
        self.read_fields += 1;
        T::strict_decode(self.parent)
    }
}

#[derive(Debug)]
pub struct StructReader<'parent, R: ReadRaw> {
    named_fields: Vec<FieldName>,
    parent: &'parent mut StrictReader<R>,
}

impl<R: ReadRaw> ReadStruct for StructReader<'_, R> {
    fn read_field<T: StrictDecode>(&mut self, field: FieldName) -> Result<T, DecodeError> {
        self.named_fields.push(field);
        T::strict_decode(self.parent)
    }
}

impl<R: ReadRaw> ReadUnion for StrictReader<R> {
    type TupleReader<'parent>
        = TupleReader<'parent, R>
    where Self: 'parent;
    type StructReader<'parent>
        = StructReader<'parent, R>
    where Self: 'parent;

    fn read_tuple<'parent, 'me, T: StrictSum>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::TupleReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent,
    {
        let mut reader = TupleReader {
            read_fields: 0,
            parent: self,
        };
        inner(&mut reader)
    }

    fn read_struct<'parent, 'me, T: StrictSum>(
        &'me mut self,
        inner: impl FnOnce(&mut Self::StructReader<'parent>) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError>
    where
        Self: 'parent,
        'me: 'parent,
    {
        let mut reader = StructReader {
            named_fields: empty!(),
            parent: self,
        };
        inner(&mut reader)
    }
}
