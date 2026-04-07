// Deterministic bitcoin commitments library.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2024 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2024 LNP/BP Standards Association. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Deterministic bitcoin commitments module.
//!
//! Deterministic bitcoin commitments are part of the client-side-validation.
//! They allow to embed commitment to extra-transaction data into a bitcoin
//! transaction in a provable way, such that it can always be proven that a
//! given transaction contains one and only one commitment of a specific type
//! for a given commitment protocol.

/// Name of the strict type library generated from the data types in this crate.
pub const LIB_NAME_BPCORE: &str = "BPCore";

pub mod anchor;
pub mod opret;
pub mod tapret;
mod proof;

pub use anchor::Anchor;
pub use proof::{DbcMethod, Method, MethodParseError, Proof};
