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

use std::str::FromStr;

use amplify::Wrapper;

use crate::stl::{AlphaCapsLodash, AlphaLodash, AlphaNumLodash, AlphaSmallLodash};
use crate::{RString, STRICT_TYPES_LIB};

pub const IDENT_MAX_LEN: usize = 100;

#[macro_export]
macro_rules! impl_ident_type {
    ($ty:ty) => {
        impl From<$ty> for String {
            #[inline]
            fn from(ident: $ty) -> String { ident.0.into() }
        }

        impl From<&'static str> for $ty {
            #[inline]
            fn from(ident: &'static str) -> Self { Self(RString::from(ident)) }
        }

        impl TryFrom<String> for $ty {
            type Error = $crate::InvalidRString;

            #[inline]
            fn try_from(s: String) -> Result<Self, Self::Error> { Self::from_str(&s) }
        }

        impl ::core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_tuple(&$crate::type_name::<Self>()).field(&self.as_str()).finish()
            }
        }

        impl ::core::borrow::Borrow<str> for $ty {
            #[inline]
            fn borrow(&self) -> &str { self.as_str() }
        }

        impl AsRef<str> for $ty {
            #[inline]
            fn as_ref(&self) -> &str { self.as_str() }
        }

        impl $ty {
            /// Returns string reference.
            #[inline]
            pub fn as_str(&self) -> &str { self.0.as_str() }
        }
    };
}

#[macro_export]
macro_rules! impl_ident_subtype {
    ($ty:ty) => {
        impl From<$ty> for $crate::Ident {
            #[inline]
            fn from(name: $ty) -> Self {
                $crate::Ident::from_str(name.as_str()).expect("ident is a superset")
            }
        }

        impl $ty {
            /// Converts to identifier name.
            #[inline]
            pub fn to_ident(&self) -> $crate::Ident { self.clone().into() }
            /// Converts into identifier name.
            #[inline]
            pub fn into_ident(self) -> $crate::Ident { self.into() }
        }
    };
}

#[derive(Wrapper, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, FromStr)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct Ident(RString<AlphaLodash, AlphaNumLodash, 1, IDENT_MAX_LEN>);

impl_ident_type!(Ident);
impl_strict_newtype!(Ident, STRICT_TYPES_LIB);

#[derive(Wrapper, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, FromStr)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct TypeName(RString<AlphaCapsLodash, AlphaNumLodash, 1, IDENT_MAX_LEN>);

impl_ident_type!(TypeName);
impl_ident_subtype!(TypeName);
impl_strict_newtype!(TypeName, STRICT_TYPES_LIB);

#[derive(Wrapper, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, FromStr)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct FieldName(RString<AlphaSmallLodash, AlphaNumLodash, 1, IDENT_MAX_LEN>);

impl_ident_type!(FieldName);
impl_ident_subtype!(FieldName);
impl_strict_newtype!(FieldName, STRICT_TYPES_LIB);

#[derive(Wrapper, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, FromStr)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct VariantName(RString<AlphaSmallLodash, AlphaNumLodash, 1, IDENT_MAX_LEN>);

impl_ident_type!(VariantName);
impl_ident_subtype!(VariantName);
impl_strict_newtype!(VariantName, STRICT_TYPES_LIB);

#[derive(Wrapper, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, From)]
#[wrapper(Deref, Display, FromStr)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", transparent)
)]
pub struct LibName(RString<AlphaCapsLodash, AlphaNumLodash, 1, IDENT_MAX_LEN>);

impl_ident_type!(LibName);
impl_ident_subtype!(LibName);
impl_strict_newtype!(LibName, STRICT_TYPES_LIB);
