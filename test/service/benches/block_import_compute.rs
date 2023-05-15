// This file is part of Cumulus.

// Copyright (C) 2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

use cumulus_test_runtime::{AccountId, GluttonCall, SudoCall};
use cumulus_test_service::construct_extrinsic;
use sc_client_api::UsageProvider;
use sp_arithmetic::Perbill;

use core::time::Duration;
use cumulus_primitives_core::ParaId;

use sc_block_builder::{BlockBuilderProvider, RecordProof};
use sp_keyring::Sr25519Keyring::Alice;

mod utils;

fn benchmark_block_import(c: &mut Criterion) {
	sp_tracing::try_init_simple();

	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");
	let para_id = ParaId::from(100);
	let tokio_handle = runtime.handle();

	let endowed_accounts = vec![AccountId::from(Alice.public())];
	let alice = runtime.block_on(
		cumulus_test_service::TestNodeBuilder::new(para_id, tokio_handle.clone(), Alice)
			// Preload all accounts with funds for the transfers
			.endowed_accounts(endowed_accounts)
			.build(),
	);
	let client = alice.client;

	// Building the very first block is around ~30x slower than any subsequent one,
	// so let's make sure it's built and imported before we benchmark anything.
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();

	let initialize_glutton = construct_extrinsic(
		&client,
		SudoCall::sudo {
			call: Box::new(
				GluttonCall::initialize_pallet { new_count: 5000, witness_count: None }.into(),
			),
		},
		Alice.into(),
		Some(0),
	);

	let compute_level = Perbill::from_percent(100);
	let storage_level = Perbill::from_percent(0);

	let set_compute = construct_extrinsic(
		&client,
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_compute { compute: compute_level.clone() }.into()),
		},
		Alice.into(),
		Some(1),
	);

	let set_storage = construct_extrinsic(
		&client,
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_storage { storage: storage_level.clone() }.into()),
		},
		Alice.into(),
		Some(2),
	);

	let mut block_builder = client.new_block(Default::default()).unwrap();
	block_builder.push(utils::extrinsic_set_time(&client)).unwrap();
	block_builder.push(utils::extrinsic_set_validation_data(parent_header)).unwrap();
	block_builder.push(initialize_glutton.into()).unwrap();
	block_builder.push(set_compute.into()).unwrap();
	block_builder.push(set_storage.into()).unwrap();
	let built_block = block_builder.build().unwrap();
	runtime.block_on(utils::import_block(&client, &built_block.block, false));

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

	let mut group = c.benchmark_group("Block import");
	group.sample_size(20);
	group.measurement_time(Duration::from_secs(120));

	group.bench_function(
		format!("(compute = {:?}, storage = {:?}) block import", compute_level, storage_level),
		|b| {
			b.to_async(&runtime).iter_batched(
				|| {},
				|_| async {
					utils::import_block(&*client, &benchmark_block.block, true).await;
				},
				BatchSize::SmallInput,
			)
		},
	);
}

criterion_group!(benches, benchmark_block_import);
criterion_main!(benches);
