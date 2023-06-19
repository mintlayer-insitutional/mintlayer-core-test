// Copyright (c) 2023 RBB S.r.l
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

use chainstate_test_framework::{create_chain_config_with_staking_pool, TestFramework};
use common::{chain::create_unittest_pos_config, primitives::Idable};
use crypto::{
    key::{KeyKind, PrivateKey},
    vrf::{VRFKeyKind, VRFPrivateKey},
};
use test_utils::random::make_seedable_rng;

use criterion::{criterion_group, criterion_main, Criterion};

pub fn pow_reorg(c: &mut Criterion) {
    let mut rng = make_seedable_rng(1111.into());
    let mut tf = TestFramework::builder(&mut rng).build();

    let common_block_id = tf.create_chain(&tf.genesis().get_id().into(), 5, &mut rng).unwrap();

    tf.create_chain(&common_block_id, 1000, &mut rng).unwrap();

    c.bench_function("PoW reorg", |b| {
        b.iter(|| {
            tf.create_chain(&common_block_id, 1000, &mut rng).unwrap();
        })
    });
}

pub fn pos_reorg(c: &mut Criterion) {
    let mut rng = make_seedable_rng(1111.into());
    let (staking_sk, staking_pk) = PrivateKey::new_from_rng(&mut rng, KeyKind::Secp256k1Schnorr);
    let (vrf_sk, vrf_pk) = VRFPrivateKey::new_from_rng(&mut rng, VRFKeyKind::Schnorrkel);

    let chain_config = create_chain_config_with_staking_pool(&mut rng, &staking_pk, &vrf_pk);
    let mut tf = TestFramework::builder(&mut rng).with_chain_config(chain_config).build();
    let target_block_time = create_unittest_pos_config().target_block_time();
    tf.progress_time_seconds_since_epoch(target_block_time.get());

    let common_block_id = tf
        .create_chain_pos(
            &tf.genesis().get_id().into(),
            5,
            &mut rng,
            &staking_sk,
            &vrf_sk,
        )
        .unwrap();

    tf.create_chain_pos(&common_block_id, 100, &mut rng, &staking_sk, &vrf_sk)
        .unwrap();

    c.bench_function("PoS reorg", |b| {
        b.iter(|| {
            tf.create_chain_pos(&common_block_id, 100, &mut rng, &staking_sk, &vrf_sk)
                .unwrap();
        })
    });
}

criterion_group!(benches, pow_reorg, pos_reorg);
criterion_main!(benches);
