// Derivation macro library for strict encoding.
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

// Coding conventions
#![recursion_limit = "256"]

//! Derivation macros for strict encoding. To learn more about the strict
//! encoding please check `strict_encoding` crate.
//!
//! # Derivation macros
//!
//! Library exports derivation macros `#[derive(`[`StrictEncode`]`)]`,
//! `#[derive(`[`StrictDecode`]`)]`, which can be added on top of any structure
//! you'd like to support string encoding (see Example section below).
//!
//! Encoding/decoding implemented by both of these macros may be configured at
//! type and individual field level using `#[strict_type(...)]` attributes.
//!
//! # Attribute
//!
//! [`StrictEncode`] and [`StrictDecode`] behavior can be customized with
//! `#[strict_encoding(...)]` attribute, which accepts different arguments
//! depending to which part of the data type it is applied.
//!
//! ## Attribute arguments at type declaration level
//!
//! Derivation macros accept `#[strict_encoding()]` attribute with the following
//! arguments:

#[macro_use]
extern crate quote;
extern crate proc_macro;
#[macro_use]
extern crate syn;
#[macro_use]
extern crate amplify_syn;

pub(crate) mod params;
mod derive_dumb;
mod derive_type;
mod derive_encode;
mod derive_decode;

use proc_macro::TokenStream;
use syn::DeriveInput;

use crate::params::StrictDerive;

/// Derives [`StrictDumb`] implementation for the type.
#[proc_macro_derive(StrictDumb, attributes(strict_type))]
pub fn derive_strict_dumb(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as DeriveInput);
    StrictDerive::try_from(derive_input)
        .and_then(|engine| engine.derive_dumb())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// Derives [`StrictType`] implementation for the type.
#[proc_macro_derive(StrictType, attributes(strict_type))]
pub fn derive_strict_type(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as DeriveInput);
    StrictDerive::try_from(derive_input)
        .and_then(|engine| engine.derive_type())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// Derives [`StrictEncode`] implementation for the type.
#[proc_macro_derive(StrictEncode, attributes(strict_type))]
pub fn derive_strict_encode(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as DeriveInput);
    StrictDerive::try_from(derive_input)
        .and_then(|engine| engine.derive_encode())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// Derives [`StrictDecode`] implementation for the type.
#[proc_macro_derive(StrictDecode, attributes(strict_type))]
pub fn derive_strict_decode(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as DeriveInput);
    StrictDerive::try_from(derive_input)
        .and_then(|engine| engine.derive_decode())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}
