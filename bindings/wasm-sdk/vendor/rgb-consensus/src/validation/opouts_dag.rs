// RGB Consensus Library: consensus layer for RGB smart contracts.
//
// SPDX-License-Identifier: Apache-2.0
//
// Copyright (C) 2025 RGB-Tools developers
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

use std::collections::HashMap;

use daggy::{Dag, NodeIndex};

use crate::{KnownTransition, OpId, Operation, Opout, Transition};

/// DAG representing the RGB operations graph
pub type OpoutsDag = Dag<Opout, ()>;

/// Index to store opout-node relations
pub type OpoutsDagIndex = HashMap<Opout, NodeIndex>;

/// Operations DAG and related opout-node index
pub type OpoutsDagData = (OpoutsDag, OpoutsDagIndex);

/// Utility struct to build an operations DAG
pub struct OpoutsDagInfo {
    dag: OpoutsDag,
    index: OpoutsDagIndex,
    cached_outputs: HashMap<OpId, Vec<NodeIndex>>,
}

impl OpoutsDagInfo {
    pub fn new() -> Self {
        Self {
            dag: Dag::new(),
            index: HashMap::new(),
            cached_outputs: HashMap::new(),
        }
    }

    pub fn register_output(&mut self, opout: Opout) -> NodeIndex {
        *self
            .index
            .entry(opout)
            .or_insert_with(|| self.dag.add_node(opout))
    }

    pub fn cache_outputs(&mut self, opid: &OpId, output_nodes: Vec<NodeIndex>) {
        self.cached_outputs.insert(*opid, output_nodes);
    }

    pub fn register_outputs(&mut self, operation: &impl Operation, opid: &OpId) {
        let mut output_nodes = Vec::new();
        for (&ty, typed_assigns) in operation.assignments().flat().iter() {
            for no in 0..typed_assigns.len_u16() {
                let k = Opout::new(*opid, ty, no);
                output_nodes.push(self.register_output(k));
            }
        }
        self.cache_outputs(opid, output_nodes);
    }

    pub fn connect_input_to_outputs_by_opid(&mut self, input: Opout, opid: &OpId) {
        if let Some(&src) = self.index.get(&input) {
            if let Some(output_nodes) = self.cached_outputs.get(opid) {
                for &dst in output_nodes {
                    let _ = self.dag.add_edge(src, dst, ());
                }
            }
        }
    }

    pub fn connect_transition(&mut self, transition: &Transition, opid: &OpId) {
        for input in &transition.inputs {
            self.connect_input_to_outputs_by_opid(input, opid);
        }
    }

    pub fn build_dag(&mut self, known_transitions: &[&KnownTransition]) {
        for KnownTransition { transition, opid } in known_transitions {
            self.register_outputs(transition, opid);
            self.connect_transition(transition, opid);
        }
    }

    pub fn to_opouts_dag_data(&self) -> OpoutsDagData { (self.dag.clone(), self.index.clone()) }
}

impl Default for OpoutsDagInfo {
    fn default() -> Self { Self::new() }
}
