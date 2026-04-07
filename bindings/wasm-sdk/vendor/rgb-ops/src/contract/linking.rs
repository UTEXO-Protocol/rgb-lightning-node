// RGB ops library for smart contracts on Bitcoin & Lightning network
//
// SPDX-License-Identifier: Apache-2.0
//
// Copyright (C) 2026 RGB-Tools developers. All rights reserved.
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

use rgb::ContractId;

use crate::contract::{IssuerWrapper, SchemaWrapper};
use crate::persistence::ContractStateRead;

/// Error derived from contract linking validation procedure
#[derive(Clone, PartialEq, Eq, Debug, Display, Error, From)]
#[display(inner)]
pub enum LinkError {
    /// Contract links to more than one Contract ID
    MultipleValues,
    /// Contract does not link to a Contract ID
    NoValue,
    /// Link between parent and child contract is broken
    ValueMismatch,
    /// Value is not a valid Contract ID
    Invalid,
}

pub trait LinkableSchemaWrapper<S: ContractStateRead>: SchemaWrapper<S> {
    fn link_to(&self) -> Result<Option<ContractId>, LinkError>;
    fn link_from(&self) -> Result<Option<ContractId>, LinkError>;
}

pub trait LinkableIssuerWrapper: IssuerWrapper {
    type Wrapper<S: ContractStateRead>: LinkableSchemaWrapper<S>;
}
