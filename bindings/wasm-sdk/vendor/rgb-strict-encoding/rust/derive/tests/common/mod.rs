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

use std::fmt::{Debug, Display, Formatter};

use strict_encoding::{StrictDecode, StrictEncode};
use strict_encoding_test::DataEncodingTestFailure;

#[derive(Display)]
#[display(inner)]
pub struct Error(pub Box<dyn std::error::Error>);

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result { Display::fmt(self, f) }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(self.0.as_ref()) }
}

impl<T> From<DataEncodingTestFailure<T>> for Error
where T: StrictEncode + StrictDecode + PartialEq + Debug + Clone + 'static
{
    fn from(err: DataEncodingTestFailure<T>) -> Self { Self(Box::new(err)) }
}

/*
impl From<strict_encoding::Error> for Error {
    fn from(err: strict_encoding::Error) -> Self { Self(Box::new(err)) }
}
*/

pub type Result = std::result::Result<(), Error>;
