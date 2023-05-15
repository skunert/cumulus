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

use cumulus_primitives_parachain_inherent::ParachainInherentData;
use cumulus_test_relay_sproof_builder::RelayStateSproofBuilder;
use cumulus_test_runtime::{BalancesCall, NodeBlock, UncheckedExtrinsic, WASM_BINARY};
use cumulus_test_service::{construct_extrinsic, Client as TestClient};
use polkadot_primitives::HeadData;
use sc_client_api::UsageProvider;

use cumulus_primitives_core::{relay_chain::AccountId, PersistedValidationData};
use sc_block_builder::BlockBuilderProvider;
use sc_consensus::{
	block_import::{BlockImportParams, ForkChoiceStrategy},
	BlockImport, ImportResult, StateAction,
};
use sc_executor::DEFAULT_HEAP_ALLOC_STRATEGY;
use sc_executor_common::runtime_blob::RuntimeBlob;
use sp_blockchain::{ApplyExtrinsicFailed::Validity, Error::ApplyExtrinsicFailed};
use sp_consensus::BlockOrigin;
use sp_core::{sr25519, Pair};
use sp_keyring::Sr25519Keyring::Alice;
use sp_runtime::{
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	AccountId32, OpaqueExtrinsic,
};

// Accounts to use for transfer transactions. Enough for 5000 transactions.
const NUM_ACCOUNTS: usize = 10000;

pub(crate) fn create_benchmark_accounts(
) -> (Vec<sr25519::Pair>, Vec<sr25519::Pair>, Vec<AccountId32>) {
	let accounts: Vec<sr25519::Pair> = (0..NUM_ACCOUNTS)
		.into_iter()
		.map(|idx| {
			Pair::from_string(&format!("{}/{}", Alice.to_seed(), idx), None)
				.expect("Creates account pair")
		})
		.collect();
	let account_ids = accounts
		.iter()
		.map(|account| AccountId::from(account.public()))
		.collect::<Vec<AccountId>>();
	let (src_accounts, dst_accounts) = accounts.split_at(NUM_ACCOUNTS / 2);
	(src_accounts.to_vec(), dst_accounts.to_vec(), account_ids)
}

pub(crate) fn extrinsic_set_time(client: &TestClient) -> OpaqueExtrinsic {
	let best_number = client.usage_info().chain.best_number;

	let timestamp = best_number as u64 * cumulus_test_runtime::MinimumPeriod::get();
	cumulus_test_runtime::UncheckedExtrinsic {
		signature: None,
		function: cumulus_test_runtime::RuntimeCall::Timestamp(pallet_timestamp::Call::set {
			now: timestamp,
		}),
	}
	.into()
}

pub(crate) fn extrinsic_set_validation_data(
	parent_header: cumulus_test_runtime::Header,
) -> OpaqueExtrinsic {
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

pub(crate) async fn import_block(
	mut client: &TestClient,
	block: &NodeBlock,
	import_existing: bool,
) {
	let mut params = BlockImportParams::new(BlockOrigin::File, block.header.clone());
	params.body = Some(block.extrinsics.clone());
	params.state_action = StateAction::Execute;
	params.fork_choice = Some(ForkChoiceStrategy::LongestChain);
	params.import_existing = import_existing;
	let import_result = client.import_block(params).await;
	assert_eq!(
		true,
		matches!(import_result, Ok(ImportResult::Imported(_))),
		"Unexpected block import result: {:?}!",
		import_result
	);
}

pub(crate) fn create_extrinsics(
	client: &TestClient,
	src_accounts: &[sr25519::Pair],
	dst_accounts: &[sr25519::Pair],
) -> (usize, Vec<OpaqueExtrinsic>) {
	// Add as many tranfer extrinsics as possible into a single block.
	let mut block_builder = client.new_block(Default::default()).unwrap();
	let mut max_transfer_count = 0;
	let mut extrinsics = Vec::new();
	// Every block needs one timestamp extrinsic.
	let time_ext = extrinsic_set_time(client);
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

	if max_transfer_count >= src_accounts.len() {
		panic!("Block could fit more transfers, increase NUM_ACCOUNTS to generate more accounts.");
	}

	(max_transfer_count, extrinsics)
}

pub(crate) fn precompile_wasm() -> Box<dyn sc_executor_common::wasm_runtime::WasmModule> {
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
