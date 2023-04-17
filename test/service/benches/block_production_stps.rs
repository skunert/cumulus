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

use cumulus_primitives_core::relay_chain::AccountId;
use kitchensink_runtime::{constants::currency::*, BalancesCall, SudoCall};
use node_cli::service::{create_extrinsic, FullClient};
use sc_block_builder::{BlockBuilderProvider, BuiltBlock, RecordProof};
use sc_client_api::execution_extensions::ExecutionStrategies;
use sc_consensus::{
	block_import::{BlockImportParams, ForkChoiceStrategy},
	BlockImport, StateAction,
};
use sc_service::{
	config::{
		BlocksPruning, DatabaseSource, KeystoreConfig, NetworkConfiguration, OffchainWorkerConfig,
		PruningMode, WasmExecutionMethod, WasmtimeInstantiationStrategy,
	},
	BasePath, Configuration, Role,
};
use sp_blockchain::{ApplyExtrinsicFailed::Validity, Error::ApplyExtrinsicFailed};
use sp_consensus::BlockOrigin;
use sp_core::{sr25519, Pair};
use sp_keyring::{
	Ed25519Keyring::{Alice, Bob},
	Sr25519Keyring,
};
use sp_runtime::{
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	OpaqueExtrinsic,
};
use tokio::runtime::Handle;

fn new_node(tokio_handle: Handle) -> node_cli::service::NewFullBase {
	let base_path = BasePath::new_temp_dir()
		.expect("getting the base path of a temporary path doesn't fail; qed");
	let root = base_path.path().to_path_buf();

	let network_config = NetworkConfiguration::new(
		Sr25519Keyring::Alice.to_seed(),
		"network/test/0.1",
		Default::default(),
		None,
	);

	let spec = Box::new(node_cli::chain_spec::development_config());

	// NOTE: We enforce the use of the WASM runtime to benchmark block production using WASM.
	let execution_strategy = sc_client_api::ExecutionStrategy::AlwaysWasm;

	let config = Configuration {
		impl_name: "BenchmarkImpl".into(),
		impl_version: "1.0".into(),
		// We don't use the authority role since that would start producing blocks
		// in the background which would mess with our benchmark.
		role: Role::Full,
		tokio_handle,
		transaction_pool: Default::default(),
		network: network_config,
		keystore: KeystoreConfig::InMemory,
		database: DatabaseSource::ParityDb { path: root.join("db") },
		trie_cache_maximum_size: Some(64 * 1024 * 1024),
		state_pruning: Some(PruningMode::ArchiveAll),
		blocks_pruning: BlocksPruning::KeepAll,
		chain_spec: spec,
		wasm_method: WasmExecutionMethod::Compiled {
			instantiation_strategy: WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
		},
		execution_strategies: ExecutionStrategies {
			syncing: execution_strategy,
			importing: execution_strategy,
			block_construction: execution_strategy,
			offchain_worker: execution_strategy,
			other: execution_strategy,
		},
		rpc_http: None,
		rpc_ws: None,
		rpc_ipc: None,
		rpc_ws_max_connections: None,
		rpc_cors: None,
		rpc_methods: Default::default(),
		rpc_max_payload: None,
		rpc_max_request_size: None,
		rpc_max_response_size: None,
		rpc_id_provider: None,
		rpc_max_subs_per_conn: None,
		ws_max_out_buffer_capacity: None,
		prometheus_config: None,
		telemetry_endpoints: None,
		default_heap_pages: None,
		offchain_worker: OffchainWorkerConfig { enabled: true, indexing_enabled: false },
		force_authoring: false,
		disable_grandpa: false,
		dev_key_seed: Some(Sr25519Keyring::Alice.to_seed()),
		tracing_targets: None,
		tracing_receiver: Default::default(),
		max_runtime_instances: 8,
		runtime_cache_size: 2,
		announce_block: true,
		base_path: Some(base_path),
		informant_output_format: Default::default(),
		wasm_runtime_overrides: None,
	};

	node_cli::service::new_full_base(config, false, |_, _| ())
		.expect("creating a full node doesn't fail")
}

const MINIMUM_PERIOD_FOR_BLOCKS: u64 = 1500;
fn extrinsic_set_time(now: u64) -> OpaqueExtrinsic {
	kitchensink_runtime::UncheckedExtrinsic {
		signature: None,
		function: kitchensink_runtime::RuntimeCall::Timestamp(pallet_timestamp::Call::set { now }),
	}
	.into()
}

fn import_block(
	mut client: &FullClient,
	built: BuiltBlock<
		node_primitives::Block,
		<FullClient as sp_api::CallApiAt<node_primitives::Block>>::StateBackend,
	>,
) {
	let mut params = BlockImportParams::new(BlockOrigin::File, built.block.header);
	params.state_action =
		StateAction::ApplyChanges(sc_consensus::StorageChanges::Changes(built.storage_changes));
	params.fork_choice = Some(ForkChoiceStrategy::LongestChain);
	futures::executor::block_on(client.import_block(params))
		.expect("importing a block doesn't fail");
}

fn extrinsic_set_balance(
	client: &FullClient,
	nonce: &mut u32,
	dst: sp_runtime::AccountId32,
) -> OpaqueExtrinsic {
	let extrinsic = create_extrinsic(
		client,
		Sr25519Keyring::Alice.pair(),
		SudoCall::sudo {
			call: Box::new(
				BalancesCall::force_set_balance { who: dst.into(), new_free: 1_000_000 * DOLLARS }
					.into(),
			),
		},
		Some(*nonce),
	);
	*nonce += 1;
	extrinsic.into()
}

fn prepare_benchmark(client: &FullClient) -> (usize, Vec<OpaqueExtrinsic>) {
	let mut src_accounts: Vec<sr25519::Pair> = Default::default();
	let mut dst_accounts: Vec<sr25519::Pair> = Default::default();
	// Create 20.000 accounts for max 10.000 transfers
	for i in 0..10000 {
		let src: sr25519::Pair = Pair::from_string(&format!("{}/{}", Alice.to_seed(), i), None)
			.expect("Creates account pair");
		let dst: sr25519::Pair = Pair::from_string(&format!("{}/{}", Bob.to_seed(), i), None)
			.expect("Creates account pair");
		src_accounts.push(src);
		dst_accounts.push(dst);
	}

	let mut block_builder = client.new_block(Default::default()).unwrap();
	let mut alice_nonce = 0;
	let time_ext = extrinsic_set_time(1);
	block_builder.push(time_ext).expect("Should be able to set time");
	for acc in src_accounts.iter().chain(dst_accounts.iter()) {
		let ex = extrinsic_set_balance(client, &mut alice_nonce, acc.public().into());
		block_builder.push(ex).expect("Should be able to set balance");
	}

	tracing::info!("Created {} accounts", src_accounts.len());
	let new_block = block_builder.build().unwrap();
	import_block(client, new_block);

	// Add as many tranfer extrinsics as possible into a single block.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	let mut max_transfer_count = 0;
	let mut extrinsics = Vec::new();
	// Every block needs one timestamp extrinsic.
	let time_ext = extrinsic_set_time(1 + MINIMUM_PERIOD_FOR_BLOCKS);
	extrinsics.push(time_ext);
	for (src, dst) in src_accounts.iter().zip(dst_accounts.iter()) {
		let extrinsic: OpaqueExtrinsic = create_extrinsic(
			client,
			src.clone(),
			BalancesCall::transfer_allow_death {
				dest: AccountId::from(dst.public()).into(),
				value: 1 * DOLLARS,
			},
			Some(0),
		)
		.into();

		match block_builder.push(extrinsic.clone()) {
			Ok(_) => {},
			Err(ApplyExtrinsicFailed(Validity(TransactionValidityError::Invalid(
				InvalidTransaction::ExhaustsResources,
			)))) => break,
			Err(error) => panic!("{}", error),
		}

		extrinsics.push(extrinsic);
		max_transfer_count += 1;
	}

	(max_transfer_count, extrinsics)
}

fn block_production(c: &mut Criterion) {
	sp_tracing::try_init_simple();

	let runtime = tokio::runtime::Runtime::new().expect("creating tokio runtime doesn't fail; qed");
	let tokio_handle = runtime.handle().clone();

	let node = new_node(tokio_handle.clone());
	let client = &*node.client;

	// Buliding the very first block is around ~30x slower than any subsequent one,
	// so let's make sure it's built and imported before we benchmark anything.
	//let mut block_builder = client.new_block(Default::default()).unwrap();
	//block_builder.push(extrinsic_set_time(1)).unwrap();
	//import_block(client, block_builder.build().unwrap());

	let (max_transfer_count, extrinsics) = prepare_benchmark(&client);

	tracing::info!("Maximum transfer count: {}", max_transfer_count);

	let mut group = c.benchmark_group("Block production");

	group.sample_size(10);
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

criterion_group!(benches, block_production);
criterion_main!(benches);
