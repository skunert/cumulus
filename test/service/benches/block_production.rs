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

use codec::Encode;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use cumulus_primitives_parachain_inherent::ParachainInherentData;
use cumulus_test_relay_sproof_builder::RelayStateSproofBuilder;
use cumulus_test_runtime::{BalancesCall, NodeBlock, UncheckedExtrinsic};
use cumulus_test_service::{construct_extrinsic, Client as TestClient};
use polkadot_primitives::HeadData;
use sc_client_api::UsageProvider;

use core::time::Duration;
use cumulus_primitives_core::{relay_chain::AccountId, ParaId, PersistedValidationData};
use sc_block_builder::{BlockBuilderProvider, BuiltBlock, RecordProof};
use sc_consensus::{
	block_import::{BlockImportParams, ForkChoiceStrategy},
	BlockImport, ImportResult, StateAction,
};
use sp_blockchain::{ApplyExtrinsicFailed::Validity, Error::ApplyExtrinsicFailed};
use sp_consensus::BlockOrigin;
use sp_core::{sr25519, Pair};
use sp_keyring::Sr25519Keyring::Alice;
use sp_runtime::{
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	AccountId32, OpaqueExtrinsic,
};

mod utils;

fn benchmark_block_production(c: &mut Criterion) {
	sp_tracing::try_init_simple();

	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");
	let tokio_handle = runtime.handle();

	// Create enough accounts to fill the block with transactions.
	// Each account should only be included in one transfer.
	let (src_accounts, dst_accounts, account_ids) = utils::create_benchmark_accounts();

	let para_id = ParaId::from(100);
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
	let set_validation_data_extrinsic = utils::extrinsic_set_validation_data(parent_header);
	block_builder.push(set_validation_data_extrinsic.clone()).unwrap();
	let built_block = block_builder.build().unwrap();
	runtime.block_on(utils::import_block(&client, &built_block.block, false));

	let (max_transfer_count, mut extrinsics) =
		utils::create_extrinsics(&client, &src_accounts, &dst_accounts);
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
	let set_validation_data_extrinsic = utils::extrinsic_set_validation_data(parent_header);
	extrinsics.push(set_validation_data_extrinsic);

	let mut group = c.benchmark_group("Block production");

	group.sample_size(20);
	group.measurement_time(Duration::from_secs(45));
	group.throughput(Throughput::Elements(max_transfer_count as u64));

	let best_hash = client.chain_info().best_hash;

	group.bench_function(
		format!("(proof = true, transfers = {}) block production", max_transfer_count),
		|b| {
			b.iter_batched(
				|| extrinsics.clone(),
				|extrinsics| {
					let mut block_builder = client
						.new_block_at(best_hash, Default::default(), RecordProof::Yes)
						.unwrap();
					for extrinsic in extrinsics {
						block_builder.push(extrinsic).unwrap();
					}
					block_builder.build().unwrap()
				},
				BatchSize::SmallInput,
			)
		},
	);

	group.bench_function(
		format!("(proof = false, transfers = {}) block production", max_transfer_count),
		|b| {
			b.iter_batched(
				|| extrinsics.clone(),
				|extrinsics| {
					let mut block_builder = client
						.new_block_at(best_hash, Default::default(), RecordProof::No)
						.unwrap();
					for extrinsic in extrinsics {
						block_builder.push(extrinsic).unwrap();
					}
					block_builder.build().unwrap()
				},
				BatchSize::SmallInput,
			)
		},
	);
}

criterion_group!(benches, benchmark_block_production);
criterion_main!(benches);
