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

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

use sc_client_api::UsageProvider;

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

	// Create enough accounts to fill the block with transactions.
	// Each account should only be included in one transfer.
	let (src_accounts, dst_accounts, account_ids) = utils::create_benchmark_accounts();

	let alice = runtime.block_on(
		cumulus_test_service::TestNodeBuilder::new(para_id, tokio_handle.clone(), Alice)
			// Preload all accounts with funds for the transfers
			.endowed_accounts(account_ids)
			.build(),
	);
	let client = alice.client;

	// Building the very first block is around ~30x slower than any subsequent one,
	// so let's make sure it's built and imported before we benchmark anything.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	block_builder.push(utils::extrinsic_set_time(&client)).unwrap();
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
	block_builder.push(utils::extrinsic_set_validation_data(parent_header)).unwrap();
	let built_block = block_builder.build().unwrap();
	runtime.block_on(utils::import_block(&client, &built_block.block, false));

	let (max_transfer_count, extrinsics) =
		utils::create_extrinsics(&client, &src_accounts, &dst_accounts);

	// Build the block we will use for benchmarking
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
	let set_validate_extrinsic = utils::extrinsic_set_validation_data(parent_header.clone());
	let mut block_builder =
		client.new_block_at(parent_hash, Default::default(), RecordProof::No).unwrap();
	block_builder.push(set_validate_extrinsic.clone()).unwrap();
	for extrinsic in extrinsics.clone() {
		block_builder.push(extrinsic).unwrap();
	}
	let benchmark_block = block_builder.build().unwrap();

	let mut group = c.benchmark_group("Block import");
	group.sample_size(20);
	group.measurement_time(Duration::from_secs(45));
	group.throughput(Throughput::Elements(max_transfer_count as u64));

	group.bench_function(format!("block import with {} transfers", max_transfer_count), |b| {
		b.to_async(&runtime).iter_batched(
			|| {},
			|_| async {
				utils::import_block(&*client, &benchmark_block.block, true).await;
			},
			BatchSize::SmallInput,
		)
	});
}

criterion_group!(benches, benchmark_block_import);
criterion_main!(benches);
