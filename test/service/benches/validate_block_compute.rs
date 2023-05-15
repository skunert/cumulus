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
use core::time::Duration;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use cumulus_primitives_core::{relay_chain::AccountId, PersistedValidationData, ValidationParams};
use cumulus_test_client::{
	generate_extrinsic_with_pair, BuildParachainBlockData, InitBlockBuilder, TestClientBuilder,
};
use cumulus_test_relay_sproof_builder::RelayStateSproofBuilder;
use cumulus_test_runtime::{GluttonCall, SudoCall};
use polkadot_primitives::HeadData;
use sc_client_api::UsageProvider;
use sc_consensus::{BlockImport, BlockImportParams, ForkChoiceStrategy, ImportResult, StateAction};
use sp_arithmetic::Perbill;
use sp_consensus::BlockOrigin;

use sp_keyring::Sr25519Keyring::Alice;
use sp_runtime::traits::Header as HeaderT;

mod utils;

async fn import_block(
	mut client: &cumulus_test_client::Client,
	built: cumulus_test_runtime::Block,
	import_existing: bool,
) {
	let mut params = BlockImportParams::new(BlockOrigin::File, built.header.clone());
	params.body = Some(built.extrinsics.clone());
	params.state_action = StateAction::Execute;
	params.fork_choice = Some(ForkChoiceStrategy::LongestChain);
	params.import_existing = import_existing;
	let import_result = client.import_block(params).await;
	assert_eq!(true, matches!(import_result, Ok(ImportResult::Imported(_))));
}

fn benchmark_block_validation(c: &mut Criterion) {
	sp_tracing::try_init_simple();
	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");

	let endowed_accounts = vec![AccountId::from(Alice.public())];
	let mut test_client_builder = TestClientBuilder::with_default_backend()
		.set_execution_strategy(sc_client_api::ExecutionStrategy::NativeElseWasm);
	let genesis_init = test_client_builder.genesis_init_mut();
	*genesis_init = cumulus_test_client::GenesisParameters { endowed_accounts };

	let client = test_client_builder.build_with_native_executor(None).0;

	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();

	let sproof_builder: RelayStateSproofBuilder = Default::default();

	let (relay_parent_storage_root, _) = sproof_builder.clone().into_state_root_and_proof();
	let validation_data = PersistedValidationData {
		relay_parent_number: 1,
		parent_head: parent_header.encode().into(),
		..Default::default()
	};

	let mut block_builder =
		client.init_block_builder(Some(validation_data.clone()), Default::default());
	let initialize_glutton = generate_extrinsic_with_pair(
		&client,
		Alice.into(),
		SudoCall::sudo {
			call: Box::new(
				GluttonCall::initialize_pallet { new_count: 5000, witness_count: None }.into(),
			),
		},
		Some(0),
	);

	let compute_level = Perbill::from_percent(100);
	let storage_level = Perbill::from_percent(0);

	let set_compute = generate_extrinsic_with_pair(
		&client,
		Alice.into(),
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_compute { compute: compute_level.clone() }.into()),
		},
		Some(1),
	);

	let set_storage = generate_extrinsic_with_pair(
		&client,
		Alice.into(),
		SudoCall::sudo {
			call: Box::new(GluttonCall::set_storage { storage: storage_level.clone() }.into()),
		},
		Some(2),
	);

	block_builder.push(initialize_glutton).unwrap();
	block_builder.push(set_compute).unwrap();
	block_builder.push(set_storage).unwrap();

	let parachain_block = block_builder.build_parachain_block(*parent_header.state_root());

	runtime.block_on(import_block(&client, parachain_block.clone().into_block(), false));

	// Build next block
	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();
	let validation_data = PersistedValidationData {
		relay_parent_number: 1,
		parent_head: parent_header.encode().into(),
		..Default::default()
	};
	let block_builder =
		client.init_block_builder(Some(validation_data.clone()), Default::default());
	let parachain_block = block_builder.build_parachain_block(*parent_header.state_root());

	tracing::info!(
		"Storage Proof Size: {}",
		parachain_block.storage_proof().encode().len() as f64 / 1024f64,
	);

	runtime.block_on(import_block(&client, parachain_block.clone().into_block(), false));
	let runtime = utils::get_wasm_module();

	let encoded_params = ValidationParams {
		block_data: cumulus_test_client::BlockData(parachain_block.clone().encode()),
		parent_head: HeadData(parent_header.encode()),
		relay_parent_number: 1,
		relay_parent_storage_root: relay_parent_storage_root.clone(),
	}
	.encode();

	let mut group = c.benchmark_group("Block validation");
	group.sample_size(20);
	group.measurement_time(Duration::from_secs(120));

	group.bench_function(
		format!("(compute = {:?}, storage = {:?}) block validation", compute_level, storage_level),
		|b| {
			b.iter_batched(
				|| runtime.new_instance().unwrap(),
				|mut instance| {
					instance.call_export("validate_block", &encoded_params).unwrap();
				},
				BatchSize::SmallInput,
			)
		},
	);
}

criterion_group!(benches, benchmark_block_validation);
criterion_main!(benches);
