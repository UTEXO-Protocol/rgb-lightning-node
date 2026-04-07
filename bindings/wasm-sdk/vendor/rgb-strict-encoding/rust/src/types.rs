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

use std::any;
use std::collections::BTreeSet;
use std::fmt::{Debug, Display};
use std::marker::PhantomData;

use crate::{LibName, TypeName, VariantName};

pub fn type_name<T>() -> String {
    fn get_ident(path: &str) -> &str {
        path.rsplit_once("::").map(|(_, n)| n.trim()).unwrap_or(path)
    }

    let name = any::type_name::<T>().replace('&', "");
    let mut ident = vec![];
    for mut arg in name.split([',', '<', '>', '(', ')']) {
        arg = arg.trim();
        if arg.is_empty() {
            continue;
        }
        ident.push(get_ident(arg));
    }
    ident.join("")
}

#[derive(Clone, Eq, PartialEq, Debug, Display, Error)]
#[display("unexpected variant {1} for enum or union {0:?}")]
pub struct VariantError<V: Debug + Display>(pub Option<String>, pub V);

impl<V: Debug + Display> VariantError<V> {
    pub fn with<T>(val: V) -> Self { VariantError(Some(type_name::<T>()), val) }
    pub fn typed(name: impl Into<String>, val: V) -> Self { VariantError(Some(name.into()), val) }
    pub fn untyped(val: V) -> Self { VariantError(None, val) }
}

pub trait StrictDumb: Sized {
    fn strict_dumb() -> Self;
}

// Marker trait: only types that implement this can get the blanket StrictDumb.
pub trait DefaultBasedStrictDumb {}

impl<T> DefaultBasedStrictDumb for Option<T> {}
impl<T, E> DefaultBasedStrictDumb for Result<T, E> {}
impl<T> DefaultBasedStrictDumb for Vec<T> {}
impl<T: Default + DefaultBasedStrictDumb> DefaultBasedStrictDumb for std::rc::Rc<T> {}
impl<T: Default + DefaultBasedStrictDumb> DefaultBasedStrictDumb for std::sync::Arc<T> {}
impl<E, const N: usize> DefaultBasedStrictDumb for [E; N] where E: Default + DefaultBasedStrictDumb {}
impl<T: ?Sized> DefaultBasedStrictDumb for &T where T: DefaultBasedStrictDumb {}
impl<T: ?Sized> DefaultBasedStrictDumb for &mut T where T: DefaultBasedStrictDumb {}
impl<T, const N: usize> DefaultBasedStrictDumb for amplify::Array<T, N> where T: Default + DefaultBasedStrictDumb
{}
impl<T, const MAX: usize> DefaultBasedStrictDumb
    for amplify::confinement::Confined<Vec<T>, 0, MAX>
{
}

impl<T> StrictDumb for T
where T: StrictType + Default + DefaultBasedStrictDumb
{
    fn strict_dumb() -> Self { Default::default() }
}

macro_rules! impl_marker_for {
    ($($t:ty),+ $(,)?) => { $( impl DefaultBasedStrictDumb for $t {} )+ };
}

impl_marker_for!(
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    bool,
    char,
    (),
    String,
    amplify::num::u1,
    amplify::num::u2,
    amplify::num::u3,
    amplify::num::u4,
    amplify::num::u5,
    amplify::num::u6,
    amplify::num::u7,
    amplify::num::u24,
    amplify::num::u40,
    amplify::num::u48,
    amplify::num::u56,
    amplify::num::u256,
    amplify::num::u512,
    amplify::num::u1024,
    amplify::num::i256,
    amplify::num::i512,
    amplify::num::i1024,
);

pub trait StrictType: Sized {
    const STRICT_LIB_NAME: &'static str;
    fn strict_name() -> Option<TypeName> { Some(tn!(type_name::<Self>())) }
}

impl<T: StrictType> StrictType for &T {
    const STRICT_LIB_NAME: &'static str = T::STRICT_LIB_NAME;
}

impl<T> StrictType for PhantomData<T> {
    const STRICT_LIB_NAME: &'static str = "";
}

pub trait StrictProduct: StrictType + StrictDumb {}

pub trait StrictTuple: StrictProduct {
    const FIELD_COUNT: u8;
    fn strict_check_fields() {
        let name = Self::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        assert_ne!(
            Self::FIELD_COUNT,
            0,
            "tuple type {} does not contain a single field defined",
            name
        );
    }

    fn strict_type_info() -> TypeInfo<Self> {
        Self::strict_check_fields();
        TypeInfo {
            lib: libname!(Self::STRICT_LIB_NAME),
            name: Self::strict_name().map(|name| tn!(name)),
            cls: TypeClass::Tuple(Self::FIELD_COUNT),
            dumb: Self::strict_dumb(),
        }
    }
}

pub trait StrictStruct: StrictProduct {
    const ALL_FIELDS: &'static [&'static str];

    fn strict_check_fields() {
        let name = Self::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        assert!(
            !Self::ALL_FIELDS.is_empty(),
            "struct type {} does not contain a single field defined",
            name
        );
        let names: BTreeSet<_> = Self::ALL_FIELDS.iter().copied().collect();
        assert_eq!(
            names.len(),
            Self::ALL_FIELDS.len(),
            "struct type {} contains repeated field names",
            name
        );
    }

    fn strict_type_info() -> TypeInfo<Self> {
        Self::strict_check_fields();
        TypeInfo {
            lib: libname!(Self::STRICT_LIB_NAME),
            name: Self::strict_name().map(|name| tn!(name)),
            cls: TypeClass::Struct(Self::ALL_FIELDS),
            dumb: Self::strict_dumb(),
        }
    }
}

pub trait StrictSum: StrictType {
    const ALL_VARIANTS: &'static [(u8, &'static str)];

    fn strict_check_variants() {
        let name = Self::strict_name().unwrap_or_else(|| tn!("__unnamed"));
        assert!(
            !Self::ALL_VARIANTS.is_empty(),
            "type {} does not contain a single variant defined",
            name
        );
        let (ords, names): (BTreeSet<_>, BTreeSet<_>) = Self::ALL_VARIANTS.iter().copied().unzip();
        assert_eq!(
            ords.len(),
            Self::ALL_VARIANTS.len(),
            "type {} contains repeated variant ids",
            name
        );
        assert_eq!(
            names.len(),
            Self::ALL_VARIANTS.len(),
            "type {} contains repeated variant names",
            name
        );
    }

    fn variant_name_by_tag(tag: u8) -> Option<VariantName> {
        Self::ALL_VARIANTS
            .iter()
            .find(|(n, _)| *n == tag)
            .map(|(_, variant_name)| vname!(*variant_name))
    }

    fn variant_ord(&self) -> u8 {
        let variant = self.variant_name();
        for (tag, name) in Self::ALL_VARIANTS {
            if *name == variant {
                return *tag;
            }
        }
        unreachable!(
            "not all variants are enumerated for {} enum in StrictUnion::all_variants \
             implementation",
            type_name::<Self>()
        )
    }
    fn variant_name(&self) -> &'static str;
}

pub trait StrictUnion: StrictSum + StrictDumb {
    fn strict_type_info() -> TypeInfo<Self> {
        Self::strict_check_variants();
        TypeInfo {
            lib: libname!(Self::STRICT_LIB_NAME),
            name: Self::strict_name().map(|name| tn!(name)),
            cls: TypeClass::Union(Self::ALL_VARIANTS),
            dumb: Self::strict_dumb(),
        }
    }
}

pub trait StrictEnum
where
    Self: StrictSum + Copy + TryFrom<u8, Error = VariantError<u8>>,
    u8: From<Self>,
{
    fn from_variant_name(name: &VariantName) -> Result<Self, VariantError<&VariantName>> {
        for (tag, n) in Self::ALL_VARIANTS {
            if *n == name.as_str() {
                return Self::try_from(*tag).map_err(|_| VariantError::with::<Self>(name));
            }
        }
        Err(VariantError::with::<Self>(name))
    }

    fn strict_type_info() -> TypeInfo<Self> {
        Self::strict_check_variants();
        TypeInfo {
            lib: libname!(Self::STRICT_LIB_NAME),
            name: Self::strict_name().map(|name| tn!(name)),
            cls: TypeClass::Enum(Self::ALL_VARIANTS),
            dumb: Self::try_from(Self::ALL_VARIANTS[0].0)
                .expect("first variant contains invalid value"),
        }
    }
}

pub enum TypeClass {
    Embedded,
    Enum(&'static [(u8, &'static str)]),
    Union(&'static [(u8, &'static str)]),
    Tuple(u8),
    Struct(&'static [&'static str]),
}

pub struct TypeInfo<T: StrictType> {
    pub lib: LibName,
    pub name: Option<TypeName>,
    pub cls: TypeClass,
    pub dumb: T,
}

#[cfg(test)]
mod test {
    use amplify::confinement::TinyVec;

    use super::*;

    #[test]
    fn name_derivation() { assert_eq!(Option::<TinyVec<u8>>::strict_name(), None) }
}
