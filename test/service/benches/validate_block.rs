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
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use cumulus_primitives_core::{relay_chain::AccountId, PersistedValidationData, ValidationParams};
use cumulus_test_client::{
	generate_extrinsic_with_pair, BuildParachainBlockData, InitBlockBuilder, TestClientBuilder,
};
use cumulus_test_relay_sproof_builder::RelayStateSproofBuilder;
use cumulus_test_runtime::{BalancesCall, UncheckedExtrinsic, WASM_BINARY};
use polkadot_primitives::HeadData;
use sc_block_builder::BlockBuilderProvider;
use sc_client_api::UsageProvider;

use sc_executor::DEFAULT_HEAP_ALLOC_STRATEGY;
use sc_executor_common::runtime_blob::RuntimeBlob;
use sp_blockchain::{ApplyExtrinsicFailed::Validity, Error::ApplyExtrinsicFailed};

use sp_core::{sr25519, Pair};

use sp_keyring::Sr25519Keyring::Alice;
use sp_runtime::{
	traits::Header as HeaderT,
	transaction_validity::{InvalidTransaction, TransactionValidityError},
};

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
			None,
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

fn benchmark_block_validation(c: &mut Criterion) {
	sp_tracing::try_init_simple();
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
	let genesis_init = test_client_builder.genesis_init_mut();
	*genesis_init = cumulus_test_client::GenesisParameters { endowed_accounts };
	let client = test_client_builder.build_with_native_executor(None).0;

	let (src_accounts, dst_accounts) = accounts.split_at(10000);
	let (max_transfer_count, extrinsics) = prepare_benchmark(&client, src_accounts, dst_accounts);

	tracing::info!("Maximum transfer count: {}", max_transfer_count);

	let mut group = c.benchmark_group("Block production");

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
	for extrinsic in extrinsics.clone() {
		block_builder.push(extrinsic).unwrap();
	}

	let parachain_block = block_builder.build_parachain_block(*parent_header.state_root());

	tracing::info!(
		"PoV size {{ header: {}kb, extrinsics: {}kb, storage_proof: {}kb }}",
		parachain_block.header().encode().len() as f64 / 1024f64,
		parachain_block.extrinsics().encode().len() as f64 / 1024f64,
		parachain_block.storage_proof().encode().len() as f64 / 1024f64,
	);
	let runtime = initialize_wasm();

	let encoded_params = ValidationParams {
		block_data: cumulus_test_client::BlockData(parachain_block.clone().encode()),
		parent_head: HeadData(parent_header.encode()),
		relay_parent_number: 1,
		relay_parent_storage_root: relay_parent_storage_root.clone(),
	}
	.encode();

	group.sample_size(20);
	group.measurement_time(Duration::from_secs(45));
	group.throughput(Throughput::Elements(max_transfer_count as u64));

	group.bench_function(format!("block validation with {} transfer", max_transfer_count), |b| {
		b.iter_batched(
			|| runtime.new_instance().unwrap(),
			|mut instance| {
				instance.call_export("validate_block", &encoded_params).unwrap();
			},
			BatchSize::SmallInput,
		)
	});
}

fn initialize_wasm() -> Box<dyn sc_executor_common::wasm_runtime::WasmModule> {
	let blob = RuntimeBlob::uncompress_if_needed(
		WASM_BINARY.expect("You need to build the WASM binaries to run the benchmark!"),
	)
	.unwrap();

	let allow_missing_func_imports = true;

	let config = sc_executor_wasmtime::Config {
		allow_missing_func_imports,
		cache_path: None,
		semantics: sc_executor_wasmtime::Semantics {
			heap_alloc_strategy: DEFAULT_HEAP_ALLOC_STRATEGY,
			instantiation_strategy: sc_executor::WasmtimeInstantiationStrategy::PoolingCopyOnWrite,
			deterministic_stack_limit: None,
			canonicalize_nans: false,
			parallel_compilation: true,
			wasm_multi_value: false,
			wasm_bulk_memory: false,
			wasm_reference_types: false,
			wasm_simd: false,
		},
	};
	let precompiled_blob =
		sc_executor_wasmtime::prepare_runtime_artifact(blob, &config.semantics).unwrap();

	let tmpdir = tempfile::tempdir().expect("jo");
	let path = tmpdir.path().join("module.bin");
	std::fs::write(&path, &precompiled_blob).unwrap();
	unsafe {
		Box::new(
			sc_executor_wasmtime::create_runtime_from_artifact::<sp_io::SubstrateHostFunctions>(
				&path, config,
			)
			.expect("works"),
		)
	}
}

criterion_group!(benches, benchmark_block_validation);
criterion_main!(benches);
