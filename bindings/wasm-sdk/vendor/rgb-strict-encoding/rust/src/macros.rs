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

#[macro_export]
macro_rules! strict_dumb {
    () => {
        $crate::StrictDumb::strict_dumb()
    };
}

#[macro_export]
macro_rules! impl_strict_newtype {
    ($ty:ident, $lib:expr) => {
        impl_strict_newtype!($ty, $lib, Self($crate::StrictDumb::strict_dumb()));
    };
    ($ty:ident, $lib:expr, $dumb:expr) => {
        impl $crate::StrictDumb for $ty {
            fn strict_dumb() -> Self { Self::from($dumb) }
        }
        impl $crate::StrictType for $ty {
            const STRICT_LIB_NAME: &'static str = $lib;
        }
        impl $crate::StrictProduct for $ty {}
        impl $crate::StrictTuple for $ty {
            const FIELD_COUNT: u8 = 1;
        }
        impl $crate::StrictEncode for $ty {
            fn strict_encode<W: $crate::TypedWrite>(&self, writer: W) -> ::std::io::Result<W> {
                writer.write_newtype::<Self>(&self.0)
            }
        }
        impl $crate::StrictDecode for $ty {
            fn strict_decode(
                reader: &mut impl $crate::TypedRead,
            ) -> Result<Self, $crate::DecodeError> {
                use $crate::ReadTuple;
                reader.read_tuple(|r| Ok(Self(r.read_field()?)))
            }
        }
    };
}

#[macro_export]
macro_rules! impl_strict_struct {
    ($ty:ty, $lib:expr; $($field:ident),+ $(,)?) => {
        impl_strict_struct!($ty, $lib; $($field => $crate::strict_dumb!()),+);
    };
    ($ty:ty, $lib:expr; $($field:ident => $dumb:expr),+ $(,)?) => {
        impl $crate::StrictDumb for $ty {
            fn strict_dumb() -> Self {
                Self {
                    $($field: $dumb),+
                }
            }
        }
        impl $crate::DefaultBasedStrictDumb for $ty { }
        impl $crate::StrictType for $ty {
            const STRICT_LIB_NAME: &'static str = $lib;
        }
        impl $crate::StrictProduct for $ty {}
        impl $crate::StrictStruct for $ty {
            const ALL_FIELDS: &'static [&'static str] = &[$(stringify!($field)),+];
        }
        impl $crate::StrictEncode for $ty {
            fn strict_encode<W: $crate::TypedWrite>(&self, writer: W) -> io::Result<W> {
                writer.write_struct::<Self>(|w| {
                    Ok(w
                        $(.write_field(fname!(stringify!($field)), &self.$field)?)+
                        .complete())
                })
            }
        }
        impl $crate::StrictDecode for $ty {
            fn strict_decode(reader: &mut impl $crate::TypedRead) -> Result<Self, $crate::DecodeError> {
                reader.read_struct(|r| {
                    $(let $field = r.read_field(fname!(stringify!($field)))?;)+
                    Ok(Self { $($field),+ })
                })
            }
        }
    };
}

#[macro_export]
macro_rules! ident {
    ($name:literal) => {
        $crate::Ident::from($name)
    };
    ($name:expr) => {
        $crate::Ident::try_from($name).expect("hardcoded parameter is not a valid identifier name")
    };
    ($fmt:literal, $($arg:expr),+) => {{
        $crate::Ident::try_from(format!($fmt, $($arg),+))
            .unwrap_or_else(|_| panic!("invalid identifier from formatter"))
    }};
}

#[macro_export]
macro_rules! tn {
    ($name:literal) => {
        $crate::TypeName::from($name)
    };
    ($name:expr) => {
        $crate::TypeName::try_from($name).expect("hardcoded parameter is not a valid type name")
    };
    ($name:literal, $($arg:expr),+) => {{
        $crate::Ident::try_from(format!($fmt, $($arg),+))
            .unwrap_or_else(|_| panic!("invalid type name from formatter"))
    }};
}

#[macro_export]
macro_rules! vname {
    ($name:literal) => {
        $crate::VariantName::from($name)
    };
    ($name:expr) => {
        $crate::VariantName::try_from($name)
            .expect("hardcoded parameter is not a valid variant name")
    };
}

#[macro_export]
macro_rules! fname {
    ($name:literal) => {
        $crate::FieldName::from($name)
    };
    ($name:expr) => {
        $crate::FieldName::try_from($name).expect("hardcoded parameter is not a valid field name")
    };
}

#[macro_export]
macro_rules! libname {
    ($name:literal) => {
        $crate::LibName::from($name)
    };
    ($name:expr) => {
        $crate::LibName::try_from($name).expect("hardcoded parameter is not a valid library name")
    };
}
