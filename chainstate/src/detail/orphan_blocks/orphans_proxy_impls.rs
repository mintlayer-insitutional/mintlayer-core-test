// Copyright (c) 2022 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://github.com/mintlayer/mintlayer-core/blob/master/LICENSE
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::{OrphanBlocks, OrphanBlocksMut, OrphansProxy};

const RECV_ERR_MSG: &str = "Failed to recv from orphan blocks proxy. This should never happen as the destruction of the proxy should end the communication; but something else did";

impl OrphanBlocks for OrphansProxy {
    fn len(&self) -> usize {
        self.call(move |o| o.len()).recv().expect(RECV_ERR_MSG)
    }

    fn is_already_an_orphan(
        &self,
        block_id: &common::primitives::Id<common::chain::Block>,
    ) -> bool {
        let block_id = *block_id;
        self.call(move |o| o.is_already_an_orphan(&block_id))
            .recv()
            .expect(RECV_ERR_MSG)
    }
}

impl OrphanBlocksMut for OrphansProxy {
    fn clear(&mut self) {
        self.call_mut(move |o| o.clear()).recv().expect(RECV_ERR_MSG)
    }

    fn add_block(
        &mut self,
        block: common::primitives::id::WithId<common::chain::Block>,
    ) -> Result<(), Box<super::OrphanAddError>> {
        self.call_mut(move |o| o.add_block(block)).recv().expect(RECV_ERR_MSG)
    }

    fn take_all_children_of(
        &mut self,
        block_id: &common::primitives::Id<common::chain::GenBlock>,
    ) -> Vec<common::primitives::id::WithId<common::chain::Block>> {
        let block_id = *block_id;
        self.call_mut(move |o| o.take_all_children_of(&block_id))
            .recv()
            .expect(RECV_ERR_MSG)
    }
}
