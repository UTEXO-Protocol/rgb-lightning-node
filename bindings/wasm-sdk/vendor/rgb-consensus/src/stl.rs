// RGB Consensus Library: consensus layer for RGB smart contracts.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2024 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2024 LNP/BP Standards Association. All rights reserved.
// Copyright (C) 2019-2024 Dr Maxim Orlovsky. All rights reserved.
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

pub use aluvm::stl::aluvm_stl;
use bitcoin::Txid;
use strict_types::stl::{bitcoin_stl, std_stl, strict_types_stl};
use strict_types::typelib::LibBuilder;
use strict_types::TypeLib;

use crate::commit_verify::{mpc, MerkleHash, MerkleNode, StrictHash, LIB_NAME_COMMIT_VERIFY};
use crate::dbc::{self, LIB_NAME_BPCORE};
use crate::txout::{self, TxPtr};
use crate::validation::DbcProof;
use crate::vm::GlobalOrd;
use crate::{
    seals, BundleId, ContractId, Genesis, OpCommitment, Schema, TransitionBundle,
    LIB_NAME_RGB_COMMIT, LIB_NAME_RGB_LOGIC,
};

pub const LIB_ID_COMMIT_VERIFY: &str =
    "stl:G28pSECj-k8LSNgX-194cjdl-MJhnIBd-P8PDPq2-mUsF6Nc#sigma-mozart-round";
/// Strict types id for the library providing data types from [`dbc`] and
/// [`seals`] crates.
pub const LIB_ID_BPCORE: &str =
    "stl:tFjXtx2y-aVOTVcD-lfw3f4W-9RLWJmn-o8_nFyL-7WpZRVo#single-college-raja";
/// Strict types id for the library providing data types for RGB consensus.
pub const LIB_ID_RGB_COMMIT: &str =
    "stl:2CKxVms~-MKFA1Er-2nOHq3q-M_5GP8o-S_z4HIe-0jqI6_Q#peace-lunch-janet";
/// Strict types id for the library providing data types for RGB consensus.
pub const LIB_ID_RGB_LOGIC: &str =
    "stl:2Z_2RMLg-~pxm3ig-_obnAGJ-5A0skqq-qMyKQjL-_MNCLU0#alamo-habitat-alpine";

pub fn commit_verify_stl() -> TypeLib {
    LibBuilder::with(libname!(LIB_NAME_COMMIT_VERIFY), [
        strict_types::stl::std_stl().to_dependency_types()
    ])
    .transpile::<MerkleHash>()
    .transpile::<MerkleNode>()
    .transpile::<StrictHash>()
    .transpile::<mpc::Commitment>()
    .transpile::<mpc::Leaf>()
    .transpile::<mpc::MerkleBlock>()
    .transpile::<mpc::MerkleConcealed>()
    .transpile::<mpc::MerkleProof>()
    .transpile::<mpc::MerkleTree>()
    .compile()
    .unwrap()
}

/// Generates strict type library providing data types from [`dbc`] and
/// [`seals`] crates.
pub fn bp_core_stl() -> TypeLib {
    LibBuilder::with(libname!(LIB_NAME_BPCORE), [
        bitcoin_stl().to_dependency_types(),
        commit_verify_stl().to_dependency_types(),
    ])
    .transpile::<dbc::Anchor<dbc::opret::OpretProof>>()
    .transpile::<dbc::Anchor<dbc::tapret::TapretProof>>()
    .transpile::<seals::SecretSeal>()
    .transpile::<txout::BlindSeal<TxPtr>>()
    .transpile::<txout::BlindSeal<Txid>>()
    .transpile::<txout::ExplicitSeal<TxPtr>>()
    .transpile::<txout::ExplicitSeal<Txid>>()
    .compile()
    .unwrap()
}

/// Generates strict type library providing data types for RGB consensus.
pub fn rgb_commit_stl() -> TypeLib {
    LibBuilder::with(libname!(LIB_NAME_RGB_COMMIT), [
        std_stl().to_dependency_types(),
        strict_types_stl().to_dependency_types(),
        commit_verify_stl().to_dependency_types(),
        bitcoin_stl().to_dependency_types(),
        bp_core_stl().to_dependency_types(),
        aluvm_stl().to_dependency_types(),
    ])
    .transpile::<BundleId>()
    .transpile::<Genesis>()
    .transpile::<OpCommitment>()
    .transpile::<Schema>()
    .transpile::<TransitionBundle>()
    .transpile::<Txid>()
    .compile()
    .unwrap()
}

/// Generates minimal library containing contract ID
pub fn rgb_contract_id_stl() -> TypeLib {
    LibBuilder::with(libname!(LIB_NAME_RGB_COMMIT), [])
        .transpile::<ContractId>()
        .compile()
        .unwrap()
}

/// Generates strict type library providing data types for RGB consensus.
pub fn rgb_logic_stl() -> TypeLib {
    LibBuilder::with(libname!(LIB_NAME_RGB_LOGIC), [
        bitcoin_stl().to_dependency_types(),
        bp_core_stl().to_dependency_types(),
        rgb_commit_stl().to_dependency_types(),
    ])
    .transpile::<DbcProof>()
    .transpile::<GlobalOrd>()
    .compile()
    .unwrap()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn commit_verify_lib_id() {
        let lib = commit_verify_stl();
        assert_eq!(lib.id().to_string(), LIB_ID_COMMIT_VERIFY);
    }

    #[test]
    fn bp_core_lib_id() {
        let lib = bp_core_stl();
        assert_eq!(lib.id().to_string(), LIB_ID_BPCORE);
    }

    #[test]
    fn commit_lib_id() {
        let lib = rgb_commit_stl();
        assert_eq!(lib.id().to_string(), LIB_ID_RGB_COMMIT);
    }

    #[test]
    fn logic_lib_id() {
        let lib = rgb_logic_stl();
        assert_eq!(lib.id().to_string(), LIB_ID_RGB_LOGIC);
    }
}
