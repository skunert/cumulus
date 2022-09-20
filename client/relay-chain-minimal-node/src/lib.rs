// Copyright 2017-2022 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

use collator_overseer::{CollatorOverseerGenArgs, NewMinimalNode};

use cumulus_relay_chain_interface::RelayChainError;
use polkadot_network_bridge::{peer_sets_info, IsAuthority};
use polkadot_node_network_protocol::{
	peer_set::PeerSetProtocolNames,
	request_response::{self, IncomingRequest, ReqProtocolNames},
};
use polkadot_node_subsystem_util::metrics::prometheus::Registry;
use polkadot_primitives::v2::CollatorPair;

use sc_authority_discovery::Service as AuthorityDiscoveryService;
use sc_network::{Event, NetworkService};
use sc_network_common::service::NetworkEventStream;
use std::sync::Arc;

use polkadot_service::{open_database, Configuration, TaskManager};

use futures::StreamExt;

use sp_runtime::traits::Block as BlockT;

mod collator_overseer;

mod network;

mod blockchain_rpc_client;
pub use blockchain_rpc_client::BlockChainRpcClient;

fn build_authority_discovery_service<Block: BlockT>(
	task_manager: &TaskManager,
	client: Arc<BlockChainRpcClient>,
	config: &Configuration,
	network: Arc<NetworkService<Block, <Block as BlockT>::Hash>>,
	prometheus_registry: Option<Registry>,
) -> AuthorityDiscoveryService {
	let auth_disc_publish_non_global_ips = config.network.allow_non_globals_in_dht;
	let authority_discovery_role = sc_authority_discovery::Role::Discover;
	let dht_event_stream = network.event_stream("authority-discovery").filter_map(|e| async move {
		match e {
			Event::Dht(e) => Some(e),
			_ => None,
		}
	});
	let (worker, service) = sc_authority_discovery::new_worker_and_service_with_config(
		sc_authority_discovery::WorkerConfig {
			publish_non_global_ips: auth_disc_publish_non_global_ips,
			// Require that authority discovery records are signed.
			strict_record_validation: true,
			..Default::default()
		},
		client,
		network.clone(),
		Box::pin(dht_event_stream),
		authority_discovery_role,
		prometheus_registry.clone(),
	);

	task_manager.spawn_handle().spawn(
		"authority-discovery-worker",
		Some("authority-discovery"),
		worker.run(),
	);
	service
}

/// Builds a minimal relay chain node. Chain data is fetched
/// via [`BlockChainRpcClient`] and fed into the overseer and its subsystems.
///
/// Instead of spawning all subsystems, this minimal node will only spawn subsystems
/// required to collate:
/// - AvailabilityRecovery
/// - CollationGeneration
/// - CollatorProtocol
/// - NetworkBridgeRx
/// - NetworkBridgeTx
/// - RuntimeApi
#[sc_tracing::logging::prefix_logs_with("Relaychain")]
pub async fn new_minimal_relay_chain(
	mut config: Configuration,
	collator_pair: CollatorPair,
	relay_chain_rpc_client: Arc<BlockChainRpcClient>,
) -> Result<NewMinimalNode, RelayChainError> {
	let role = config.role.clone();

	// Use the given RPC node as bootnode, since we do not have a chain spec with valid boot nodes
	let mut boot_node_address = relay_chain_rpc_client.local_listen_addresses().await?;
	config.network.boot_nodes.append(&mut boot_node_address);

	let task_manager = {
		let registry = config.prometheus_config.as_ref().map(|cfg| &cfg.registry);
		TaskManager::new(config.tokio_handle.clone(), registry)?
	};

	let prometheus_registry = config.prometheus_registry().cloned();

	let genesis_hash = relay_chain_rpc_client
		.block_get_hash(Some(0))
		.await
		.expect("Crash here if no genesis is available")
		.unwrap_or_default();

	let peer_set_protocol_names =
		PeerSetProtocolNames::new(genesis_hash, config.chain_spec.fork_id());
	{
		let is_authority = if role.is_authority() { IsAuthority::Yes } else { IsAuthority::No };
		config
			.network
			.extra_sets
			.extend(peer_sets_info(is_authority, &peer_set_protocol_names));
	}

	let request_protocol_names = ReqProtocolNames::new(genesis_hash, config.chain_spec.fork_id());
	let (collation_req_receiver, available_data_req_receiver, pov_req_receiver, chunk_req_receiver) =
		build_request_response_protocol_receivers(&request_protocol_names, &mut config);
	let (network, network_starter) =
		network::build_collator_network(network::BuildCollatorNetworkParams {
			config: &config,
			client: relay_chain_rpc_client.clone(),
			spawn_handle: task_manager.spawn_handle(),
			genesis_hash,
		})?;

	let authority_discovery_service = build_authority_discovery_service(
		&task_manager,
		relay_chain_rpc_client.clone(),
		&config,
		network.clone(),
		prometheus_registry.clone(),
	);

	let parachains_db = open_database(&config.database)?;

	let overseer_args = CollatorOverseerGenArgs {
		runtime_client: relay_chain_rpc_client.clone(),
		network_service: network.clone(),
		authority_discovery_service,
		collation_req_receiver,
		available_data_req_receiver,
		registry: prometheus_registry.as_ref(),
		spawner: task_manager.spawn_handle(),
		collator_pair,
		req_protocol_names: request_protocol_names,
		peer_set_protocol_names,
		parachains_db,
		availability_config: polkadot_service::create_availability_config(),
		pov_req_receiver,
		chunk_req_receiver,
	};

	let overseer_handle = collator_overseer::spawn_overseer(
		overseer_args,
		&task_manager,
		relay_chain_rpc_client.clone(),
	)?;

	network_starter.start_network();

	Ok(NewMinimalNode { task_manager, overseer_handle, network })
}

fn build_request_response_protocol_receivers(
	request_protocol_names: &ReqProtocolNames,
	config: &mut Configuration,
) -> (
	request_response::IncomingRequestReceiver<request_response::v1::CollationFetchingRequest>,
	request_response::IncomingRequestReceiver<request_response::v1::AvailableDataFetchingRequest>,
	request_response::IncomingRequestReceiver<request_response::v1::PoVFetchingRequest>,
	request_response::IncomingRequestReceiver<request_response::v1::ChunkFetchingRequest>,
) {
	let (collation_req_receiver, cfg) =
		IncomingRequest::get_config_receiver(request_protocol_names);
	config.network.request_response_protocols.push(cfg);
	let (available_data_req_receiver, cfg) =
		IncomingRequest::get_config_receiver(request_protocol_names);
	config.network.request_response_protocols.push(cfg);
	let (pov_req_receiver, cfg) = IncomingRequest::get_config_receiver(request_protocol_names);
	config.network.request_response_protocols.push(cfg);
	let (chunk_req_receiver, cfg) = IncomingRequest::get_config_receiver(request_protocol_names);
	config.network.request_response_protocols.push(cfg);
	(collation_req_receiver, available_data_req_receiver, pov_req_receiver, chunk_req_receiver)
}
