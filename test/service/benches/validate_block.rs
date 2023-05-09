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

use codec::{Decode, Encode};
use core::time::Duration;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use cumulus_primitives_core::{
	relay_chain::AccountId, ParaId, ParachainBlockData, PersistedValidationData, ValidationParams,
};
use cumulus_primitives_parachain_inherent::ParachainInherentData;
use cumulus_test_client::{
	generate_extrinsic_with_pair, BuildParachainBlockData, ExecutorResult, InitBlockBuilder,
	TestClientBuilder, ValidationResult,
};
use cumulus_test_relay_sproof_builder::RelayStateSproofBuilder;
use cumulus_test_runtime::{
	BalancesCall, Block, Header, NodeBlock, UncheckedExtrinsic, WASM_BINARY,
};
use polkadot_primitives::HeadData;
use sc_block_builder::{BlockBuilderProvider, BuiltBlock, RecordProof};
use sc_client_api::UsageProvider;
use sc_consensus::{
	block_import::{BlockImportParams, ForkChoiceStrategy},
	BlockImport, ImportResult, StateAction,
};
use sc_executor::{HeapAllocStrategy, WasmExecutionMethod, WasmExecutor};
use sc_executor_common::runtime_blob::RuntimeBlob;
use sp_blockchain::{ApplyExtrinsicFailed::Validity, Error::ApplyExtrinsicFailed};
use sp_consensus::BlockOrigin;
use sp_core::{sr25519, Pair};
use sp_io::TestExternalities;
use sp_keyring::Sr25519Keyring::Alice;
use sp_runtime::{
	traits::{Block as BlockT, Header as HeaderT},
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	AccountId32, BuildStorage, OpaqueExtrinsic, Storage,
};

fn extrinsic_set_time(now: u64) -> UncheckedExtrinsic {
	cumulus_test_runtime::UncheckedExtrinsic {
		signature: None,
		function: cumulus_test_runtime::RuntimeCall::Timestamp(pallet_timestamp::Call::set { now }),
	}
}

fn extrinsic_set_validation_data(
	parent_header: cumulus_test_runtime::Header,
) -> UncheckedExtrinsic {
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
}

fn prepare_benchmark(
	client: &cumulus_test_client::Client,
	src_accounts: &[sr25519::Pair],
	dst_accounts: &[sr25519::Pair],
) -> (usize, Vec<UncheckedExtrinsic>) {
	// Add as many tranfer extrinsics as possible into a single block.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	let mut max_transfer_count = 0;
	let mut extrinsics = Vec::new();

	for (src, dst) in src_accounts.iter().zip(dst_accounts.iter()) {
		let extrinsic: UncheckedExtrinsic = generate_extrinsic_with_pair(
			client,
			src.clone(),
			BalancesCall::transfer_keep_alive {
				dest: AccountId::from(dst.public()).into(),
				value: 10000,
			},
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

	let mut test_client_builder = TestClientBuilder::with_default_backend()
		.set_execution_strategy(sc_client_api::ExecutionStrategy::AlwaysWasm);
	let mut genesis_init = test_client_builder.genesis_init_mut();
	*genesis_init = cumulus_test_client::GenesisParameters { endowed_accounts };
	let client = test_client_builder.build_with_native_executor(None).0;

	let (src_accounts, dst_accounts) = accounts.split_at(10000);
	let (max_transfer_count, extrinsics) = prepare_benchmark(&client, src_accounts, dst_accounts);

	tracing::info!("Maximum transfer count: {}", max_transfer_count);

	let mut group = c.benchmark_group("Block production");

	group.sample_size(20);
	group.measurement_time(Duration::from_secs(45));
	group.throughput(Throughput::Elements(max_transfer_count as u64));

	let parent_hash = client.usage_info().chain.best_hash;
	let parent_header = client.header(parent_hash).expect("Just fetched this hash.").unwrap();

	let sproof_builder: RelayStateSproofBuilder = Default::default();
	let (relay_parent_storage_root, _) = sproof_builder.clone().into_state_root_and_proof();
	let mut validation_data = PersistedValidationData {
		relay_parent_number: 1,
		parent_head: parent_header.encode().into(),
		..Default::default()
	};

	let mut block_builder =
		client.init_block_builder(Some(validation_data.clone()), Default::default());
	validation_data.relay_parent_storage_root = relay_parent_storage_root;
	for extrinsic in extrinsics.clone() {
		block_builder.push(extrinsic).unwrap();
	}
	let parachain_block = block_builder.build_parachain_block(*parent_header.state_root());

	let heap_pages = HeapAllocStrategy::Static { extra_pages: 1024 };
	let executor = WasmExecutor::<sp_io::SubstrateHostFunctions>::builder()
		.with_execution_method(WasmExecutionMethod::Compiled {
			instantiation_strategy: sc_executor::WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
		})
		.with_max_runtime_instances(1)
		.with_runtime_cache_size(2)
		.with_onchain_heap_alloc_strategy(heap_pages)
		.with_offchain_heap_alloc_strategy(heap_pages)
		.build();

	// let expected = parachain_block.header().clone();
	// let res_header =
	// 	call_validate_block(parent_header, parachain_block, relay_parent_storage_root.clone).expect("jo");
	// assert_eq!(expected, res_header);
	group.bench_function(format!("{} imports (no proof)", max_transfer_count), |b| {
		b.iter_batched(
			|| (parent_header.clone(), parachain_block.clone(), relay_parent_storage_root.clone()),
			|(parent_header, block, storage_root)| {
				call_validate_block(&executor, parent_header, block, storage_root).expect("jo");
			},
			BatchSize::SmallInput,
		)
	});
}

fn call_validate_block(
	executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
	parent_head: cumulus_test_runtime::Header,
	parachain_block: ParachainBlockData<Block>,
	relay_parent_storage_root: cumulus_test_runtime::Hash,
) -> cumulus_test_client::ExecutorResult<cumulus_test_runtime::Header> {
	let head_data_encoded = validate_block_with_executor(
		executor,
		ValidationParams {
			block_data: cumulus_test_client::BlockData(parachain_block.encode()),
			parent_head: HeadData(parent_head.encode()),
			relay_parent_number: 1,
			relay_parent_storage_root,
		},
		WASM_BINARY.expect("You need to build the WASM binaries to run the tests!"),
	)
	.map(|v| v.head_data.0);
	head_data_encoded.map(|v| Header::decode(&mut &v[..]).expect("Decodes `Header`."))
}

/// Call `validate_block` in the given `wasm_blob`.
pub fn validate_block_with_executor(
	executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
	validation_params: ValidationParams,
	wasm_blob: &[u8],
) -> ExecutorResult<ValidationResult> {
	let mut ext = TestExternalities::default();
	let mut ext_ext = ext.ext();
	executor
		.uncached_call(
			RuntimeBlob::uncompress_if_needed(wasm_blob).expect("RuntimeBlob uncompress & parse"),
			&mut ext_ext,
			false,
			"validate_block",
			&validation_params.encode(),
		)
		.map(|v| ValidationResult::decode(&mut &v[..]).expect("Decode `ValidationResult`."))
}
criterion_group!(benches, validate_block);
criterion_main!(benches);
