use crate::BlockChainRPCClient;
use futures::{FutureExt, Stream, StreamExt};
use polkadot_service::{runtime_traits::BlockIdTo, BlockT, HeaderMetadata, HeaderT};
use sc_client_api::{BlockBackend, BlockchainEvents, HeaderBackend, ProofProvider};
use sc_consensus::ImportQueue;
use sc_network::{
	block_request_handler,
	light_client_requests::{self, handler::LightClientRequestHandler},
	state_request_handler::{self, StateRequestHandler},
	NetworkService,
};
use sc_service::{error::Error, Configuration, NetworkStarter, SpawnTaskHandle};
use sp_consensus::block_validation::DefaultBlockAnnounceValidator;
use std::{pin::Pin, sync::Arc};

#[async_trait::async_trait]
pub trait BlockchainRPCEvents<Block: BlockT> {
	async fn finality_stream(
		&self,
	) -> Pin<Box<dyn Stream<Item = <Block as BlockT>::Header> + Send>>;

	async fn import_stream(&self) -> Pin<Box<dyn Stream<Item = <Block as BlockT>::Header> + Send>>;

	async fn best_stream(&self) -> Pin<Box<dyn Stream<Item = <Block as BlockT>::Header> + Send>>;
}

pub struct BuildCollatorNetworkParams<'a, TImpQu, TCl> {
	/// The service configuration.
	pub config: &'a Configuration,
	/// A shared client returned by `new_full_parts`.
	pub client: Arc<TCl>,
	/// A handle for spawning tasks.
	pub spawn_handle: SpawnTaskHandle,
	/// An import queue.
	pub import_queue: TImpQu,
}

/// Build the network service, the network status sinks and an RPC sender.
pub fn build_collator_network<TBl, TImpQu, TCl>(
	params: BuildCollatorNetworkParams<TImpQu, TCl>,
) -> Result<(Arc<NetworkService<TBl, <TBl as BlockT>::Hash>>, NetworkStarter), Error>
where
	TBl: BlockT,
	TCl: HeaderMetadata<TBl, Error = sp_blockchain::Error>
		+ BlockBackend<TBl>
		+ BlockIdTo<TBl, Error = sp_blockchain::Error>
		+ ProofProvider<TBl>
		+ HeaderBackend<TBl>
		+ BlockchainEvents<TBl>
		+ BlockchainRPCEvents<TBl>
		+ 'static,
	TImpQu: ImportQueue<TBl> + 'static,
{
	let BuildCollatorNetworkParams { config, client, spawn_handle, import_queue } = params;

	let transaction_pool_adapter = Arc::new(sc_network::config::EmptyTransactionPool {});

	let protocol_id = config.protocol_id();

	let block_announce_validator = Box::new(DefaultBlockAnnounceValidator);

	let block_request_protocol_config =
		block_request_handler::generate_protocol_config(&protocol_id);

	let state_request_protocol_config =
		state_request_handler::generate_protocol_config(&protocol_id);

	let light_client_request_protocol_config =
		light_client_requests::generate_protocol_config(&protocol_id);

	let mut network_params = sc_network::config::Params {
		role: config.role.clone(),
		executor: {
			let spawn_handle = Clone::clone(&spawn_handle);
			Some(Box::new(move |fut| {
				spawn_handle.spawn("libp2p-node", Some("networking"), fut);
			}))
		},
		transactions_handler_executor: {
			let spawn_handle = Clone::clone(&spawn_handle);
			Box::new(move |fut| {
				spawn_handle.spawn("network-transactions-handler", Some("networking"), fut);
			})
		},
		network_config: config.network.clone(),
		chain: client.clone(),
		transaction_pool: transaction_pool_adapter as _,
		import_queue: Box::new(import_queue),
		protocol_id,
		block_announce_validator,
		metrics_registry: config.prometheus_config.as_ref().map(|config| config.registry.clone()),
		block_request_protocol_config,
		state_request_protocol_config,
		warp_sync: None,
		light_client_request_protocol_config,
	};

	let network_mut = sc_network::NetworkWorker::new(network_params)?;
	let network = network_mut.service().clone();

	let future = build_network_collator_future(network_mut, client);

	// TODO: [skunert] Remove this comment
	// TODO: Normally, one is supposed to pass a list of notifications protocols supported by the
	// node through the `NetworkConfiguration` struct. But because this function doesn't know in
	// advance which components, such as GrandPa or Polkadot, will be plugged on top of the
	// service, it is unfortunately not possible to do so without some deep refactoring. To bypass
	// this problem, the `NetworkService` provides a `register_notifications_protocol` method that
	// can be called even after the network has been initialized. However, we want to avoid the
	// situation where `register_notifications_protocol` is called *after* the network actually
	// connects to other peers. For this reason, we delay the process of the network future until
	// the user calls `NetworkStarter::start_network`.
	//
	// This entire hack should eventually be removed in favour of passing the list of protocols
	// through the configuration.
	//
	// See also https://github.com/paritytech/substrate/issues/6827
	let (network_start_tx, network_start_rx) = futures_channel::oneshot::channel();

	// The network worker is responsible for gathering all network messages and processing
	// them. This is quite a heavy task, and at the time of the writing of this comment it
	// frequently happens that this future takes several seconds or in some situations
	// even more than a minute until it has processed its entire queue. This is clearly an
	// issue, and ideally we would like to fix the network future to take as little time as
	// possible, but we also take the extra harm-prevention measure to execute the networking
	// future using `spawn_blocking`.
	spawn_handle.spawn_blocking("network-worker", Some("networking"), async move {
		if network_start_rx.await.is_err() {
			tracing::warn!(
				"The NetworkStart returned as part of `build_network` has been silently dropped"
			);
			// This `return` might seem unnecessary, but we don't want to make it look like
			// everything is working as normal even though the user is clearly misusing the API.
			return
		}

		future.await
	});

	Ok((network, NetworkStarter::new(network_start_tx)))
}

/// Builds a never-ending future that continuously polls the network.
///
/// The `status_sink` contain a list of senders to send a periodic network status to.
async fn build_network_collator_future<
	B: BlockT,
	H: sc_network::ExHashT,
	C: BlockchainRPCEvents<B>
		+ HeaderBackend<B>
		+ ProofProvider<B>
		+ HeaderMetadata<B, Error = sp_blockchain::Error>
		+ BlockBackend<B>,
>(
	mut network: sc_network::NetworkWorker<B, H, C>,
	client: Arc<C>,
) {
	let mut imported_blocks_stream = client.import_stream().await.fuse();

	// Stream of finalized blocks reported by the client.
	let mut finality_notification_stream = client.finality_stream().await.fuse();
	let mut new_best_notification_stream = client.best_stream().await.fuse();

	loop {
		futures::select! {
			// // List of blocks that the client has imported.
			// notification = imported_blocks_stream.next() => {
			// 	let notification = match notification {
			// 		Some(n) => n,
			// 		// If this stream is shut down, that means the client has shut down, and the
			// 		// most appropriate thing to do for the network future is to shut down too.
			// 		None => return,
			// 	};

			// 	// TODO skunert No need to announce anything
			// 	if announce_imported_blocks {
			// 		network.service().announce_block(notification.hash(), None);
			// 	}
			// }

			notification = new_best_notification_stream.next() => {
				let notification = match notification {
					Some(n) => n,
					// If this stream is shut down, that means the client has shut down, and the
					// most appropriate thing to do for the network future is to shut down too.
					None => return,
				};

					network.service().new_best_block_imported(
						notification.hash(),
						notification.number().clone(),
					);
			}

			// List of blocks that the client has finalized.
			notification = finality_notification_stream.select_next_some() => {
				network.on_block_finalized(notification.hash(), notification);
			}

			// Answer incoming RPC requests.

			// The network worker has done something. Nothing special to do, but could be
			// used in the future to perform actions in response of things that happened on
			// the network.
			_ = (&mut network).fuse() => {}
		}
	}
}
