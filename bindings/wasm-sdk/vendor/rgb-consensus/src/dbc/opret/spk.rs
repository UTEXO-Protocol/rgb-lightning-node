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

use bitcoin::blockdata::opcodes::all::OP_RETURN;
use bitcoin::ScriptBuf;

use crate::commit_verify::mpc::Commitment;
use crate::commit_verify::{EmbedCommitProof, EmbedCommitVerify, EmbedVerifyError};
use crate::dbc::opret::{OpretError, OpretFirst, OpretProof};

impl EmbedCommitProof<Commitment, ScriptBuf, OpretFirst> for OpretProof {
    fn restore_original_container(
        &self,
        commit_container: &ScriptBuf,
    ) -> Result<ScriptBuf, EmbedVerifyError<OpretError>> {
        if !commit_container.is_op_return() {
            return Err(OpretError::NoOpretOutput.into());
        }
        if commit_container.len() != 34 {
            return Err(OpretError::InvalidOpretScript.into());
        }
        let mut script = ScriptBuf::new();
        script.push_opcode(OP_RETURN);
        Ok(script)
    }
}

impl EmbedCommitVerify<Commitment, OpretFirst> for ScriptBuf {
    type Proof = OpretProof;
    type CommitError = OpretError;

    fn embed_commit(&mut self, msg: &Commitment) -> Result<Self::Proof, Self::CommitError> {
        if !self.is_op_return() {
            return Err(OpretError::NoOpretOutput);
        }
        if self.len() != 1 {
            return Err(OpretError::InvalidOpretScript);
        }
        *self = ScriptBuf::new_op_return(msg.to_byte_array());
        Ok(OpretProof::default())
    }
}
