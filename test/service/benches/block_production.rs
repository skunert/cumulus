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
use cumulus_test_runtime::{BalancesCall, Block, NodeBlock, UncheckedExtrinsic};
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
	traits::Block as BlockT,
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	OpaqueExtrinsic,
};

fn extrinsic_set_time(now: u64) -> OpaqueExtrinsic {
	cumulus_test_runtime::UncheckedExtrinsic {
		signature: None,
		function: cumulus_test_runtime::RuntimeCall::Timestamp(pallet_timestamp::Call::set { now }),
	}
	.into()
}

fn extrinsic_set_validation_data(parent_header: cumulus_test_runtime::Header) -> OpaqueExtrinsic {
	let mut sproof_builder = RelayStateSproofBuilder::default();
	sproof_builder.para_id = 100.into();
	let parent_head = HeadData(parent_header.encode());
	let (relay_parent_storage_root, relay_chain_state) = sproof_builder.into_state_root_and_proof();
	let data = ParachainInherentData {
		validation_data: PersistedValidationData {
			parent_head,
			relay_parent_number: 10,
			relay_parent_storage_root,
			max_pov_size: 10000,
		},
		relay_chain_state,
		downward_messages: Default::default(),
		horizontal_messages: Default::default(),
	};

	cumulus_test_runtime::UncheckedExtrinsic {
		signature: None,
		function: cumulus_test_runtime::RuntimeCall::ParachainSystem(
			cumulus_pallet_parachain_system::Call::set_validation_data { data },
		),
	}
	.into()
}

fn prepare_benchmark(
	client: &TestClient,
	src_accounts: &[sr25519::Pair],
	dst_accounts: &[sr25519::Pair],
) -> (usize, Vec<OpaqueExtrinsic>) {
	// Add as many tranfer extrinsics as possible into a single block.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	let mut max_transfer_count = 0;
	let mut extrinsics = Vec::new();
	// Every block needs one timestamp extrinsic.
	let time_ext = extrinsic_set_time(1500);
	extrinsics.push(time_ext);

	for (src, dst) in src_accounts.iter().zip(dst_accounts.iter()) {
		let extrinsic: UncheckedExtrinsic = construct_extrinsic(
			client,
			BalancesCall::transfer_keep_alive {
				dest: AccountId::from(dst.public()).into(),
				value: 10000,
			},
			src.clone(),
			Some(0),
		);

		match block_builder.push(extrinsic.clone().into()) {
			Ok(_) => {},
			Err(ApplyExtrinsicFailed(Validity(TransactionValidityError::Invalid(
				InvalidTransaction::ExhaustsResources,
			)))) => break,
			Err(error) => panic!("{}", error),
		}

		extrinsics.push(extrinsic.into());
		max_transfer_count += 1;
	}

	(max_transfer_count, extrinsics)
}

async fn import_block(
	mut client: &TestClient,
	built: BuiltBlock<
		NodeBlock,
		<TestClient as sp_api::CallApiAt<node_primitives::Block>>::StateBackend,
	>,
	import_existing: bool,
) {
	let mut params = BlockImportParams::new(BlockOrigin::File, built.block.header.clone());
	params.body = Some(built.block.extrinsics.clone());
	params.state_action = StateAction::Execute;
	params.fork_choice = Some(ForkChoiceStrategy::LongestChain);
	params.import_existing = import_existing;
	let import_result = client.import_block(params).await;
	assert_eq!(true, matches!(import_result, Ok(ImportResult::Imported(_))));
}

fn validate_block(c: &mut Criterion) {
	sp_tracing::try_init_simple();

	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");
	let para_id = ParaId::from(100);
	let tokio_handle = runtime.handle();
	let accounts: Vec<sr25519::Pair> = (0..20000)
		.into_iter()
		.map(|idx| {
			Pair::from_string(&format!("{}/{}", Alice.to_seed(), idx), None)
				.expect("Creates account pair")
		})
		.collect();
	let endowed_accounts = accounts
		.iter()
		.map(|account| AccountId::from(account.public()))
		.collect::<Vec<AccountId>>();

	let alice = runtime.block_on(
		cumulus_test_service::TestNodeBuilder::new(para_id, tokio_handle.clone(), Alice)
			.endowed_accounts(endowed_accounts)
			.build(),
	);
	let client = alice.client;

	// Buliding the very first block is around ~30x slower than any subsequent one,
	// so let's make sure it's built and imported before we benchmark anything.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	block_builder.push(extrinsic_set_time(0)).unwrap();
	let parent_hash = dbg!(client.usage_info().chain.best_hash);
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
	let set_validation_data_extrinsic = extrinsic_set_validation_data(parent_header);
	block_builder.push(set_validation_data_extrinsic.clone()).unwrap();
	runtime.block_on(import_block(&client, block_builder.build().unwrap(), false));

	let (src_accounts, dst_accounts) = accounts.split_at(10000);
	let (max_transfer_count, mut extrinsics) =
		prepare_benchmark(&client, src_accounts, dst_accounts);
	extrinsics.push(set_validation_data_extrinsic);

	tracing::info!("Maximum transfer count: {}", max_transfer_count);

	let mut group = c.benchmark_group("Block production");

	group.sample_size(20);
	group.measurement_time(Duration::from_secs(45));
	group.throughput(Throughput::Elements(max_transfer_count as u64));

	let best_hash = client.chain_info().best_hash;

	group.bench_function(format!("{} transfers (no proof)", max_transfer_count), |b| {
		b.iter_batched(
			|| extrinsics.clone(),
			|extrinsics| {
				let mut block_builder =
					client.new_block_at(best_hash, Default::default(), RecordProof::No).unwrap();
				for extrinsic in extrinsics {
					block_builder.push(extrinsic).unwrap();
				}
				block_builder.build().unwrap()
			},
			BatchSize::SmallInput,
		)
	});

	group.bench_function(format!("{} transfers (with proof)", max_transfer_count), |b| {
		b.iter_batched(
			|| extrinsics.clone(),
			|extrinsics| {
				let mut block_builder =
					client.new_block_at(best_hash, Default::default(), RecordProof::Yes).unwrap();
				for extrinsic in extrinsics {
					block_builder.push(extrinsic).unwrap();
				}
				block_builder.build().unwrap()
			},
			BatchSize::SmallInput,
		)
	});
}

criterion_group!(benches, validate_block);
criterion_main!(benches);
