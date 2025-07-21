use reth_discv4::Discv4ConfigBuilder;
use reth_network::{
    EthNetworkPrimitives, NetworkConfig, NetworkEvent, NetworkEventListenerProvider,
    NetworkManager, PeersInfo,
};
use reth_network_api::events::{PeerEvent, SessionInfo};
use reth_provider::noop::NoopProvider;
use reth_tracing::{
    LayerInfo, LogFormat, RethTracer, Tracer, tracing_subscriber::filter::LevelFilter,
};
use secp256k1::{SecretKey, rand};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_stream::StreamExt;
use tracing::{info, warn};

mod chain_config;
mod peer;

#[tokio::main]
async fn main() {
    let _ = RethTracer::new()
        .with_stdout(LayerInfo::new(
            LogFormat::Terminal,
            LevelFilter::INFO.to_string(),
            "".to_string(),
            Some("always".to_string()),
        ))
        .init();

    let local_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 30303);

    let secret_key = SecretKey::new(&mut rand::thread_rng());

    let bsc_boot_nodes = chain_config::bootnodes::bsc_mainnet_nodes();

    let state_manager = peer::blockstate::BlockStateManager::new(0);

    let (event_sender, mut event_receiver) =
        mpsc::unbounded_channel::<peer::blockstate::BlockEvent>();

    let block_importer = peer::blockstate::SmartBlockImporter::new(event_sender);

    let net_cfg = NetworkConfig::builder(secret_key)
        .boot_nodes(bsc_boot_nodes.clone())
        .set_head(chain_config::bsc::head())
        .with_pow()
        .listener_addr(local_addr)
        .eth_rlpx_handshake(Arc::new(peer::handshake::BscHandshake::default()))
        .block_import(Box::new(block_importer))
        .build(NoopProvider::eth(
            Arc::new(chain_config::bsc::bsc_mainnet()),
        ));

    let net_cfg = net_cfg.set_discovery_v4(
        Discv4ConfigBuilder::default()
            .add_boot_nodes(bsc_boot_nodes)
            .lookup_interval(Duration::from_millis(500))
            .build(),
    );
    let net_manager = NetworkManager::<EthNetworkPrimitives>::new(net_cfg)
        .await
        .unwrap();

    let net_handle = net_manager.handle().clone();
    let mut network_events = net_handle.event_listener();

    tokio::spawn(net_manager);

    info!("BSC P2P network started, listening and requesting blocks...");

    let state_for_timer = state_manager.clone();
    let handle_for_timer = net_handle.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(10));
        loop {
            interval.tick().await;

            state_for_timer.cleanup_expired_requests();

            let connected_peers = state_for_timer.peerset.lock().unwrap();
            if !connected_peers.is_empty() {
                drop(connected_peers);
                state_for_timer.request_next_block(&handle_for_timer);
            }
        }
    });

    loop {
        tokio::select! {
            network_event = network_events.next() => {
                match network_event {
                    Some(NetworkEvent::ActivePeerSession { info, .. }) => {
                        let SessionInfo { status, client_version, peer_id, .. } = info;

                        state_manager.add_peer(peer_id);

                        info!(
                            peers = %net_handle.num_connected_peers(),
                            %peer_id,
                            chain = %status.chain,
                            best_block = %status.blockhash,
                            ?status.total_difficulty,
                            ?client_version,
                            "new node connected"
                        );

                        state_manager.request_next_block(&net_handle);
                    }
                    Some(NetworkEvent::Peer(PeerEvent::SessionClosed { peer_id, reason })) => {
                        state_manager.remove_peer(&peer_id);

                        info!(
                            peers = %net_handle.num_connected_peers(),
                            %peer_id,
                            ?reason,
                            "node connection closed"
                        );
                    }
                    Some(_) => {
                    }
                    None => {
                        warn!("network event stream ended");
                        break;
                    }
                }
            }

            block_event = event_receiver.recv() => {
                match block_event {
                    Some(peer::blockstate::BlockEvent::NewBlock { peer_id, block_number, block_hash, transaction_count }) => {
                        info!(
                            %peer_id,
                            block_number = block_number,
                            block_hash = %block_hash,
                            transaction_count = transaction_count,
                            current_height = %state_manager.get_current_height(),
                            "process new block event"
                        );

                        state_manager.process_received_block(block_number);
                    }
                    Some(peer::blockstate::BlockEvent::NewBlockHashes { peer_id, block_numbers }) => {
                        info!(
                            %peer_id,
                            block_count = block_numbers.len(),
                            current_height = %state_manager.get_current_height(),
                            "process block hashes event"
                        );

                        state_manager.process_block_hashes(&block_numbers, &net_handle);
                    }
                    None => {
                        warn!("block event stream ended");
                        break;
                    }
                }
            }
        }
    }
}
