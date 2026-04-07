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

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::io::Sink;
use std::marker::PhantomData;

use amplify::confinement::U64 as U64MAX;
use amplify::WriteCounter;

use crate::{
    DefineEnum, DefineStruct, DefineTuple, DefineUnion, FieldName, LibName, StrictEncode,
    StrictEnum, StrictStruct, StrictSum, StrictTuple, StrictUnion, TypeName, TypedParent,
    TypedWrite, Variant, VariantName, WriteEnum, WriteRaw, WriteStruct, WriteTuple, WriteUnion,
    LIB_EMBEDDED,
};

// TODO: Move to amplify crate
#[derive(Clone, Debug)]
pub struct ConfinedWriter<W: io::Write> {
    count: usize,
    limit: usize,
    writer: W,
}

impl<W: io::Write> From<W> for ConfinedWriter<W> {
    fn from(writer: W) -> Self {
        Self {
            count: 0,
            limit: usize::MAX,
            writer,
        }
    }
}

impl<W: io::Write> ConfinedWriter<W> {
    pub fn with(limit: usize, writer: W) -> Self {
        Self {
            count: 0,
            limit,
            writer,
        }
    }

    pub fn unconfine(self) -> W { self.writer }
}

impl<W: io::Write> io::Write for ConfinedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.count + buf.len() > self.limit {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let count = self.writer.write(buf)?;
        self.count += count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> { self.writer.flush() }
}

#[derive(Clone, Debug)]
pub struct StreamWriter<W: io::Write>(ConfinedWriter<W>);

impl<W: io::Write> StreamWriter<W> {
    pub fn new<const MAX: usize>(inner: W) -> Self { Self(ConfinedWriter::with(MAX, inner)) }
    pub fn unconfine(self) -> W { self.0.unconfine() }
}

impl<W: io::Write> WriteRaw for StreamWriter<W> {
    fn write_raw<const MAX_LEN: usize>(&mut self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        use io::Write;
        self.0.write_all(bytes.as_ref())?;
        Ok(())
    }
}

impl StreamWriter<Vec<u8>> {
    pub fn in_memory<const MAX: usize>() -> Self { Self::new::<MAX>(vec![]) }
}

impl StreamWriter<WriteCounter> {
    pub fn counter<const MAX: usize>() -> Self { Self::new::<MAX>(WriteCounter::default()) }
}

impl StreamWriter<Sink> {
    pub fn sink<const MAX: usize>() -> Self { Self::new::<MAX>(Sink::default()) }
}

#[derive(Debug, From)]
pub struct StrictWriter<W: WriteRaw>(W);

impl StrictWriter<StreamWriter<Vec<u8>>> {
    pub fn in_memory<const MAX: usize>() -> Self { Self(StreamWriter::in_memory::<MAX>()) }
}

impl StrictWriter<StreamWriter<WriteCounter>> {
    pub fn counter<const MAX: usize>() -> Self { Self(StreamWriter::counter::<MAX>()) }
}

impl StrictWriter<StreamWriter<Sink>> {
    pub fn sink<const MAX: usize>() -> Self { Self(StreamWriter::sink::<MAX>()) }
}

impl<W: WriteRaw> StrictWriter<W> {
    pub fn with(writer: W) -> Self { Self(writer) }
    pub fn unbox(self) -> W { self.0 }
}

impl<W: WriteRaw> TypedWrite for StrictWriter<W> {
    type TupleWriter = StructWriter<W, Self>;
    type StructWriter = StructWriter<W, Self>;
    type UnionDefiner = UnionWriter<W>;
    type RawWriter = W;

    unsafe fn raw_writer(&mut self) -> &mut Self::RawWriter { &mut self.0 }

    fn write_union<T: StrictUnion>(
        self,
        inner: impl FnOnce(Self::UnionDefiner) -> io::Result<Self>,
    ) -> io::Result<Self> {
        let writer = UnionWriter::with::<T>(self);
        inner(writer)
    }

    fn write_enum<T: StrictEnum>(self, value: T) -> io::Result<Self>
    where u8: From<T> {
        let mut writer = UnionWriter::with::<T>(self);
        for (_, name) in T::ALL_VARIANTS {
            writer = writer.define_variant(vname!(*name));
        }
        writer = DefineEnum::complete(writer);
        writer = writer.write_variant(vname!(value.variant_name()))?;
        Ok(WriteEnum::complete(writer))
    }

    fn write_tuple<T: StrictTuple>(
        self,
        inner: impl FnOnce(Self::TupleWriter) -> io::Result<Self>,
    ) -> io::Result<Self> {
        let writer = StructWriter::tuple::<T>(self);
        inner(writer)
    }

    fn write_struct<T: StrictStruct>(
        self,
        inner: impl FnOnce(Self::StructWriter) -> io::Result<Self>,
    ) -> io::Result<Self> {
        let writer = StructWriter::structure::<T>(self);
        inner(writer)
    }
}

#[derive(Debug)]
pub struct StructWriter<W: WriteRaw, P: StrictParent<W>> {
    lib: LibName,
    name: Option<TypeName>,
    named_fields: Vec<FieldName>,
    tuple_fields: Option<u8>,
    parent: P,
    cursor: usize,
    _phantom: PhantomData<W>,
}

impl<W: WriteRaw, P: StrictParent<W>> StructWriter<W, P> {
    pub fn structure<T: StrictStruct>(parent: P) -> Self {
        StructWriter {
            lib: libname!(T::STRICT_LIB_NAME),
            name: T::strict_name(),
            named_fields: T::ALL_FIELDS.iter().map(|name| fname!(*name)).collect(),
            tuple_fields: None,
            parent,
            cursor: 0,
            _phantom: default!(),
        }
    }

    pub fn tuple<T: StrictTuple>(parent: P) -> Self {
        StructWriter {
            lib: libname!(T::STRICT_LIB_NAME),
            name: T::strict_name(),
            named_fields: empty!(),
            tuple_fields: Some(T::FIELD_COUNT),
            parent,
            cursor: 0,
            _phantom: default!(),
        }
    }

    pub fn unnamed(parent: P, tuple: bool) -> Self {
        StructWriter {
            lib: libname!(LIB_EMBEDDED),
            name: None,
            named_fields: empty!(),
            tuple_fields: if tuple { Some(0) } else { None },
            parent,
            cursor: 0,
            _phantom: default!(),
        }
    }

    pub fn is_tuple(&self) -> bool { self.tuple_fields.is_some() }

    pub fn is_struct(&self) -> bool { !self.is_tuple() }

    pub fn named_fields(&self) -> &[FieldName] {
        debug_assert!(self.tuple_fields.is_none(), "tuples do not contain named fields");
        self.named_fields.as_slice()
    }

    pub fn fields_count(&self) -> u8 { self.tuple_fields.unwrap_or(self.named_fields.len() as u8) }

    pub fn name(&self) -> &str { self.name.as_ref().map(|n| n.as_str()).unwrap_or("<unnamed>") }

    pub fn into_parent(self) -> P { self.parent }

    fn write_value(mut self, value: &impl StrictEncode) -> io::Result<Self> {
        let (mut writer, remnant) = self.parent.into_write_split();
        writer = value.strict_encode(writer)?;
        self.parent = P::from_write_split(writer, remnant);
        Ok(self)
    }
}

impl<W: WriteRaw, P: StrictParent<W>> DefineStruct for StructWriter<W, P> {
    type Parent = P;
    fn define_field<T: StrictEncode>(mut self, field: FieldName) -> Self {
        assert!(
            !self.named_fields.contains(&field),
            "field '{:#}' is already defined as a part of '{}'",
            field,
            self.name()
        );
        self.named_fields.push(field);
        self
    }
    fn complete(self) -> P {
        assert!(
            !self.named_fields.is_empty(),
            "struct '{}' does not have fields defined",
            self.name()
        );
        self.parent
    }
}

impl<W: WriteRaw, P: StrictParent<W>> WriteStruct for StructWriter<W, P> {
    type Parent = P;
    fn write_field(mut self, _field: FieldName, value: &impl StrictEncode) -> io::Result<Self> {
        debug_assert!(self.tuple_fields.is_none(), "using struct method on tuple");
        /* TODO: Propagate information about the fields at the parent
        debug_assert!(
            !self.named_fields.is_empty(),
            "struct without fields {} asks to write value for field '{field}'",
            self.name()
        );
        assert_eq!(
            &self.named_fields[self.cursor],
            &field,
            "field '{:#}' was not defined for '{}' or is written outside of the order",
            field,
            self.name()
        );
         */
        self.cursor += 1;
        self.write_value(value)
    }
    fn complete(self) -> P {
        /* TODO: Propagate information about the fields at the parent
        assert_eq!(
            self.cursor,
            self.named_fields.len(),
            "not all fields were written for '{}'",
            self.name()
        );
         */
        self.parent
    }
}

impl<W: WriteRaw, P: StrictParent<W>> DefineTuple for StructWriter<W, P> {
    type Parent = P;
    fn define_field<T: StrictEncode>(mut self) -> Self {
        self.tuple_fields
            .as_mut()
            .map(|count| *count += 1)
            .expect("calling tuple method on struct");
        self
    }
    fn complete(self) -> P {
        assert_ne!(
            self.tuple_fields.expect("tuple defined as struct"),
            0,
            "tuple '{}' does not have fields defined",
            self.name()
        );
        debug_assert!(self.named_fields.is_empty(), "tuple '{}' defined as struct", self.name());
        self.parent
    }
}

impl<W: WriteRaw, P: StrictParent<W>> WriteTuple for StructWriter<W, P> {
    type Parent = P;
    fn write_field(mut self, value: &impl StrictEncode) -> io::Result<Self> {
        /* TODO: Propagate information about number of fields at the parent
        assert!(
            self.tuple_fields.expect("writing tuple field to structure") as usize > self.cursor,
            "writing more unnamed fields to the tuple {} than was defined",
            self.name()
        );
         */
        self.cursor += 1;
        self.write_value(value)
    }
    fn complete(self) -> P {
        assert_ne!(self.cursor, 0, "tuple '{}' does not have any fields written", self.name());
        /* TODO: Propagate information about number of fields at the parent
        assert_eq!(
            Some(self.cursor as u8),
            self.unnamed_fields,
            "not all fields were written for '{}'",
            self.name()
        );
         */
        debug_assert!(self.named_fields.is_empty(), "tuple '{}' written as struct", self.name());
        self.parent
    }
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum VariantType {
    Unit,
    Tuple,
    Struct,
}

// TODO: Collect data about defined variant types and check them on write
#[derive(Debug)]
pub struct UnionWriter<W: WriteRaw> {
    lib: LibName,
    name: Option<TypeName>,
    declared_variants: BTreeMap<u8, VariantName>,
    declared_index: BTreeMap<VariantName, u8>,
    defined_variant: BTreeMap<Variant, VariantType>,
    parent: StrictWriter<W>,
    written: bool,
    parent_ident: Option<TypeName>,
}

impl UnionWriter<StreamWriter<Sink>> {
    pub fn sink() -> Self {
        UnionWriter {
            lib: libname!(LIB_EMBEDDED),
            name: None,
            declared_variants: empty!(),
            declared_index: empty!(),
            defined_variant: empty!(),
            parent: StrictWriter::sink::<U64MAX>(),
            written: false,
            parent_ident: None,
        }
    }
}

impl<W: WriteRaw> UnionWriter<W> {
    pub fn with<T: StrictSum>(parent: StrictWriter<W>) -> Self {
        UnionWriter {
            lib: libname!(T::STRICT_LIB_NAME),
            name: T::strict_name(),
            declared_variants: T::ALL_VARIANTS
                .iter()
                .map(|(tag, name)| (*tag, vname!(*name)))
                .collect(),
            declared_index: T::ALL_VARIANTS
                .iter()
                .map(|(tag, name)| (vname!(*name), *tag))
                .collect(),
            defined_variant: empty!(),
            parent,
            written: false,
            parent_ident: None,
        }
    }

    pub fn is_written(&self) -> bool { self.written }

    pub fn variants(&self) -> &BTreeMap<Variant, VariantType> { &self.defined_variant }

    pub fn name(&self) -> &str { self.name.as_ref().map(|n| n.as_str()).unwrap_or("<unnamed>") }

    pub fn tag_by_name(&self, name: &VariantName) -> u8 {
        *self
            .declared_index
            .get(name)
            .unwrap_or_else(|| panic!("unknown variant `{name}` for the enum `{}`", self.name()))
    }

    fn _define_variant(mut self, name: VariantName, variant_type: VariantType) -> Self {
        let tag = self.tag_by_name(&name);
        let variant = Variant::named(tag, name);
        assert!(
            self.defined_variant.insert(variant.clone(), variant_type).is_none(),
            "variant '{:#}' is already defined as a part of '{}'",
            &variant,
            self.name()
        );
        self
    }

    fn _write_variant(mut self, name: VariantName, variant_type: VariantType) -> io::Result<Self> {
        let (variant, t) =
            self.defined_variant.iter().find(|(f, _)| f.name == name).unwrap_or_else(|| {
                panic!("variant '{:#}' was not defined in '{}'", &name, self.name())
            });
        assert_eq!(
            *t,
            variant_type,
            "variant '{:#}' in '{}' must be a {:?} while it is written as {:?}",
            &variant,
            self.name(),
            t,
            variant_type
        );
        assert!(!self.written, "multiple attempts to write variants of '{}'", self.name());
        self.written = true;
        self.parent = variant.tag.strict_encode(self.parent)?;
        Ok(self)
    }

    fn _complete_definition(self) -> Self {
        let declared = self.declared_variants.values().map(|v| v.as_str()).collect::<BTreeSet<_>>();
        let defined = self.defined_variant.keys().map(|v| v.name.as_str()).collect::<BTreeSet<_>>();
        assert_eq!(
            declared,
            defined,
            "unit or enum '{}' hasn't defined all of its declared variants. Elements skipped: {:?}",
            self.name(),
            declared.difference(&defined)
        );
        assert!(
            !self.defined_variant.is_empty(),
            "unit or enum '{}' does not have any fields defined",
            self.name()
        );
        self
    }

    fn _complete_write(self) -> StrictWriter<W> {
        assert!(self.written, "not a single variant is written for '{}'", self.name());
        self.parent
    }
}

impl<W: WriteRaw> DefineUnion for UnionWriter<W> {
    type Parent = StrictWriter<W>;
    type TupleDefiner = StructWriter<W, Self>;
    type StructDefiner = StructWriter<W, Self>;
    type UnionWriter = UnionWriter<W>;

    fn define_unit(self, name: VariantName) -> Self {
        self._define_variant(name, VariantType::Unit)
    }
    fn define_tuple(
        mut self,
        name: VariantName,
        inner: impl FnOnce(Self::TupleDefiner) -> Self,
    ) -> Self {
        self = self._define_variant(name, VariantType::Tuple);
        let definer = StructWriter::unnamed(self, true);
        inner(definer)
    }
    fn define_struct(
        mut self,
        name: VariantName,
        inner: impl FnOnce(Self::StructDefiner) -> Self,
    ) -> Self {
        self = self._define_variant(name, VariantType::Struct);
        let definer = StructWriter::unnamed(self, false);
        inner(definer)
    }
    fn complete(self) -> Self::UnionWriter { self._complete_definition() }
}

impl<W: WriteRaw> WriteUnion for UnionWriter<W> {
    type Parent = StrictWriter<W>;
    type TupleWriter = StructWriter<W, Self>;
    type StructWriter = StructWriter<W, Self>;

    fn write_unit(self, name: VariantName) -> io::Result<Self> {
        self._write_variant(name, VariantType::Unit)
    }
    fn write_tuple(
        mut self,
        name: VariantName,
        inner: impl FnOnce(Self::TupleWriter) -> io::Result<Self>,
    ) -> io::Result<Self> {
        self = self._write_variant(name, VariantType::Tuple)?;
        let writer = StructWriter::unnamed(self, true);
        inner(writer)
    }
    fn write_struct(
        mut self,
        name: VariantName,
        inner: impl FnOnce(Self::StructWriter) -> io::Result<Self>,
    ) -> io::Result<Self> {
        self = self._write_variant(name, VariantType::Struct)?;
        let writer = StructWriter::unnamed(self, false);
        inner(writer)
    }
    fn complete(self) -> Self::Parent { self._complete_write() }
}

impl<W: WriteRaw> DefineEnum for UnionWriter<W> {
    type Parent = StrictWriter<W>;
    type EnumWriter = UnionWriter<W>;
    fn define_variant(self, name: VariantName) -> Self {
        self._define_variant(name, VariantType::Unit)
    }
    fn complete(self) -> Self::EnumWriter { self._complete_definition() }
}

impl<W: WriteRaw> WriteEnum for UnionWriter<W> {
    type Parent = StrictWriter<W>;
    fn write_variant(self, name: VariantName) -> io::Result<Self> {
        self._write_variant(name, VariantType::Unit)
    }
    fn complete(self) -> Self::Parent { self._complete_write() }
}

pub trait StrictParent<W: WriteRaw>: TypedParent {
    type Remnant;
    fn from_write_split(writer: StrictWriter<W>, remnant: Self::Remnant) -> Self;
    fn into_write_split(self) -> (StrictWriter<W>, Self::Remnant);
}
impl<W: WriteRaw> TypedParent for StrictWriter<W> {}
impl<W: WriteRaw> TypedParent for UnionWriter<W> {}
impl<W: WriteRaw> StrictParent<W> for StrictWriter<W> {
    type Remnant = ();
    fn from_write_split(writer: StrictWriter<W>, _: Self::Remnant) -> Self { writer }
    fn into_write_split(self) -> (StrictWriter<W>, Self::Remnant) { (self, ()) }
}
impl<W: WriteRaw> StrictParent<W> for UnionWriter<W> {
    type Remnant = UnionWriter<StreamWriter<Sink>>;
    fn from_write_split(writer: StrictWriter<W>, remnant: Self::Remnant) -> Self {
        Self {
            lib: remnant.lib,
            name: remnant.name,
            declared_variants: remnant.declared_variants,
            declared_index: remnant.declared_index,
            defined_variant: remnant.defined_variant,
            parent: writer,
            written: remnant.written,
            parent_ident: remnant.parent_ident,
        }
    }
    fn into_write_split(self) -> (StrictWriter<W>, Self::Remnant) {
        let remnant = UnionWriter {
            lib: self.lib,
            name: self.name,
            declared_variants: self.declared_variants,
            declared_index: self.declared_index,
            defined_variant: self.defined_variant,
            parent: StrictWriter::sink::<U64MAX>(),
            written: self.written,
            parent_ident: self.parent_ident,
        };
        (self.parent, remnant)
    }
}

pub trait SplitParent {
    type Parent: TypedParent;
    type Remnant;
    fn from_parent_split(parent: Self::Parent, remnant: Self::Remnant) -> Self;
    fn into_parent_split(self) -> (Self::Parent, Self::Remnant);
}
impl<W: WriteRaw, P: StrictParent<W>> SplitParent for StructWriter<W, P> {
    type Parent = P;
    type Remnant = StructWriter<StreamWriter<Sink>, ParentDumb>;
    fn from_parent_split(parent: P, remnant: Self::Remnant) -> Self {
        Self {
            lib: remnant.lib,
            name: remnant.name,
            named_fields: remnant.named_fields,
            tuple_fields: remnant.tuple_fields,
            parent,
            cursor: remnant.cursor,
            _phantom: none!(),
        }
    }
    fn into_parent_split(self) -> (P, Self::Remnant) {
        let remnant = StructWriter::<StreamWriter<Sink>, ParentDumb> {
            lib: self.lib,
            name: self.name,
            named_fields: self.named_fields,
            tuple_fields: self.tuple_fields,
            parent: none!(),
            cursor: self.cursor,
            _phantom: none!(),
        };
        (self.parent, remnant)
    }
}

#[derive(Default)]
pub struct ParentDumb;
impl TypedParent for ParentDumb {}
impl<W: WriteRaw> StrictParent<W> for ParentDumb {
    type Remnant = ();
    fn from_write_split(_: StrictWriter<W>, _: Self::Remnant) -> Self { unreachable!() }
    fn into_write_split(self) -> (StrictWriter<W>, Self::Remnant) { unreachable!() }
}
