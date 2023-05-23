// This file is part of Cumulus.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

use cumulus_test_runtime::{GluttonCall, NodeBlock, SudoCall};
use cumulus_test_service::{construct_extrinsic, Client as TestClient};
use sc_client_api::UsageProvider;
use sp_api::{Core, ProvideRuntimeApi};
use sp_arithmetic::Perbill;

use core::time::Duration;
use cumulus_primitives_core::ParaId;

use frame_system_rpc_runtime_api::AccountNonceApi;
use sc_block_builder::{BlockBuilderProvider, RecordProof};
use sp_keyring::Sr25519Keyring::Alice;

mod utils;

fn benchmark_block_import(c: &mut Criterion) {
	sp_tracing::try_init_simple();

	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");
	let para_id = ParaId::from(100);
	let tokio_handle = runtime.handle();

	let alice = runtime.block_on(
		cumulus_test_service::TestNodeBuilder::new(para_id, tokio_handle.clone(), Alice).build(),
	);
	let client = alice.client;

	let mut group = c.benchmark_group("Block import");
	group.sample_size(20);
	group.measurement_time(Duration::from_secs(120));

	let mut initialize_glutton_pallet = true;
	for (compute_percent, storage_percent) in &[
		(Perbill::from_percent(100), Perbill::from_percent(0)),
		(Perbill::from_percent(100), Perbill::from_percent(100)),
	] {
		let block = set_glutton_parameters(
			&client,
			initialize_glutton_pallet,
			compute_percent,
			compute_percent,
		);
		initialize_glutton_pallet = false;

		runtime.block_on(utils::import_block(&client, &block, false));

		// Build the block we will use for benchmarking
		let parent_hash = client.usage_info().chain.best_hash;
		let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
		let mut block_builder =
			client.new_block_at(parent_hash, Default::default(), RecordProof::No).unwrap();
		block_builder
			.push(utils::extrinsic_set_validation_data(parent_header.clone()).clone())
			.unwrap();
		block_builder.push(utils::extrinsic_set_time(&client)).unwrap();
		let benchmark_block = block_builder.build().unwrap();

		group.bench_function(
			format!(
				"(compute = {:?}, storage = {:?}) block import",
				compute_percent, storage_percent
			),
			|b| {
				b.iter_batched(
					|| benchmark_block.block.clone(),
					|block| {
						client.runtime_api().execute_block(parent_hash, block).unwrap();
					},
					BatchSize::SmallInput,
				)
			},
		);
	}
}

fn set_glutton_parameters(
	client: &TestClient,
	initialize: bool,
	compute_percent: &Perbill,
	storage_percent: &Perbill,
) -> NodeBlock {
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();

	let mut last_nonce = client
		.runtime_api()
		.account_nonce(parent_hash, Alice.into())
		.expect("Fetching account nonce works; qed");

	let mut extrinsics = vec![];
	if initialize {
		// Initialize the pallet
		extrinsics.push(construct_extrinsic(
			client,
			SudoCall::sudo {
				call: Box::new(
					GluttonCall::initialize_pallet { new_count: 5000, witness_count: None }.into(),
				),
			},
			Alice.into(),
			Some(last_nonce),
		));
		last_nonce += 1;
	}

	// Set compute weight that should be consumed per block
	let set_compute = construct_extrinsic(
		client,
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_compute { compute: *compute_percent }.into()),
		},
		Alice.into(),
		Some(last_nonce),
	);
	last_nonce += 1;
	extrinsics.push(set_compute);

	// Set storage weight that should be consumed per block
	let set_storage = construct_extrinsic(
		client,
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_storage { storage: *storage_percent }.into()),
		},
		Alice.into(),
		Some(last_nonce),
	);
	extrinsics.push(set_storage);

	let mut block_builder = client.new_block(Default::default()).unwrap();
	block_builder.push(utils::extrinsic_set_time(client)).unwrap();
	block_builder.push(utils::extrinsic_set_validation_data(parent_header)).unwrap();
	for extrinsic in extrinsics {
		block_builder.push(extrinsic.into()).unwrap();
	}

	let built_block = block_builder.build().unwrap();
	built_block.block
}

criterion_group!(benches, benchmark_block_import);
criterion_main!(benches);
