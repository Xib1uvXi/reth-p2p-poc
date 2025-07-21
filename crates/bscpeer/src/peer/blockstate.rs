use reth_network_peers::PeerId;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::{info, warn};

use reth_eth_wire::{GetBlockHeaders, HeadersDirection};
use reth_eth_wire_types::BlockHashOrNumber;
use reth_network::import::{BlockImport, BlockImportEvent, NewBlockEvent};
use reth_network::{EthNetworkPrimitives, NetworkHandle};
use reth_network_api::PeerRequest;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum BlockEvent {
    NewBlock {
        peer_id: PeerId,
        block_number: u64,
        block_hash: String,
        transaction_count: usize,
    },
    NewBlockHashes {
        peer_id: PeerId,
        block_numbers: Vec<u64>,
    },
}

#[derive(Debug, Clone)]
pub struct BlockStateManager {
    pub current_height: Arc<Mutex<u64>>,
    pub peerset: Arc<Mutex<Vec<PeerId>>>,
    /// 等待的区块请求
    pub pending_requests: Arc<Mutex<HashMap<u64, bool>>>,
    pub received_blocks: Arc<Mutex<HashSet<u64>>>,
}

impl BlockStateManager {
    pub fn new(starting_height: u64) -> Self {
        Self {
            current_height: Arc::new(Mutex::new(starting_height)),
            peerset: Arc::new(Mutex::new(Vec::new())),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            received_blocks: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn add_peer(&self, peer_id: PeerId) {
        let mut peers = self.peerset.lock().unwrap();
        if !peers.contains(&peer_id) {
            peers.push(peer_id);
            info!(%peer_id, "peerset add new peer");
        }
    }

    pub fn remove_peer(&self, peer_id: &PeerId) {
        let mut peers = self.peerset.lock().unwrap();
        peers.retain(|p| p != peer_id);
        info!(%peer_id, "peerset remove peer");
    }

    pub fn get_current_height(&self) -> u64 {
        *self.current_height.lock().unwrap()
    }

    pub fn update_height(&self, new_height: u64) -> bool {
        let mut current = self.current_height.lock().unwrap();
        if new_height > *current {
            let old_height = *current;
            *current = new_height;
            info!(
                old_height = old_height,
                new_height = new_height,
                "update block height"
            );
            true
        } else {
            false
        }
    }

    pub fn add_received_block(&self, block_number: u64) {
        let mut received = self.received_blocks.lock().unwrap();
        received.insert(block_number);
    }

    pub fn is_block_received(&self, block_number: u64) -> bool {
        let received = self.received_blocks.lock().unwrap();
        received.contains(&block_number)
    }

    pub fn request_block_by_number(
        &self,
        block_number: u64,
        network_handle: &NetworkHandle<EthNetworkPrimitives>,
    ) {
        let peers = self.peerset.lock().unwrap();
        if let Some(peer_id) = peers.first() {
            {
                let mut pending = self.pending_requests.lock().unwrap();
                if pending.contains_key(&block_number) {
                    return;
                }
                pending.insert(block_number, true);
            }

            let (response_tx, _response_rx) = oneshot::channel();

            let request = GetBlockHeaders {
                start_block: BlockHashOrNumber::Number(block_number),
                limit: 1,
                skip: 0,
                direction: HeadersDirection::Rising,
            };

            let peer_request = PeerRequest::GetBlockHeaders {
                request,
                response: response_tx,
            };

            network_handle.send_request(*peer_id, peer_request);
            info!(block_number = block_number, %peer_id, "request block");
        } else {
            warn!("no available peer to request block {}", block_number);
        }
    }

    pub fn request_next_block(&self, network_handle: &NetworkHandle<EthNetworkPrimitives>) {
        let current_height = self.get_current_height();
        let next_height = current_height + 1;
        self.request_block_by_number(next_height, network_handle);
    }

    pub fn check_and_request_missing_blocks(
        &self,
        received_block_number: u64,
        network_handle: &NetworkHandle<EthNetworkPrimitives>,
    ) {
        let current_height = self.get_current_height();

        if received_block_number > current_height + 1 {
            info!(
                current_height = current_height,
                received_block = received_block_number,
                gap = received_block_number - current_height - 1,
                "detect block gap, start request missing blocks"
            );

            let start = current_height + 1;
            let end = std::cmp::min(start + 5, received_block_number);

            for missing_block in start..end {
                if !self.is_block_received(missing_block) {
                    self.request_block_by_number(missing_block, network_handle);
                }
            }
        }
    }

    /// 处理收到的区块
    pub fn process_received_block(
        &self,
        block_number: u64,
    ) {
        {
            let mut pending = self.pending_requests.lock().unwrap();
            pending.remove(&block_number);
        }

        self.add_received_block(block_number);

        // let current_height = self.get_current_height();

        // if block_number == current_height + 1 {
        //     self.update_height(block_number);
        //     info!(
        //         block_number = block_number,
        //         "receive continuous block, height updated"
        //     );

        //     self.request_next_block(network_handle);
        // } else if block_number > current_height + 1 {
        //     self.check_and_request_missing_blocks(block_number, network_handle);
        // } else {
        //     info!(
        //         block_number = block_number,
        //         current_height = current_height,
        //         "receive old block or duplicate block"
        //     );
        // }
    }

    pub fn process_block_hashes(
        &self,
        block_numbers: &[u64],
        network_handle: &NetworkHandle<EthNetworkPrimitives>,
    ) {
        let current_height = self.get_current_height();

        for &block_number in block_numbers {
            if block_number > current_height && !self.is_block_received(block_number) {
                self.request_block_by_number(block_number, network_handle);
            }
        }
    }

    pub fn cleanup_expired_requests(&self) {
        let mut pending = self.pending_requests.lock().unwrap();
        if pending.len() > 100 {
            // 如果待处理请求太多，清理一些旧的
            let current_height = self.get_current_height();
            pending.retain(|&block_num, _| block_num > current_height.saturating_sub(50));
            info!(
                "cleanup expired block requests, current pending requests: {}",
                pending.len()
            );
        }
    }
}

#[derive(Debug)]
pub struct SmartBlockImporter {
    event_sender: mpsc::UnboundedSender<BlockEvent>,
}

impl SmartBlockImporter {
    pub fn new(event_sender: mpsc::UnboundedSender<BlockEvent>) -> Self {
        Self { event_sender }
    }
}

impl BlockImport<reth_eth_wire::NewBlock> for SmartBlockImporter {
    fn on_new_block(
        &mut self,
        peer_id: PeerId,
        incoming_block: NewBlockEvent<reth_eth_wire::NewBlock>,
    ) {
        match incoming_block {
            NewBlockEvent::Block(block_msg) => {
                let block = &block_msg.block.block;
                let block_number = block.header.number;

                info!(
                    peer_id = %peer_id,
                    block_hash = %block_msg.hash,
                    block_number = %block_number,
                    parent_hash = %block.header.parent_hash,
                    timestamp = %block.header.timestamp,
                    gas_limit = %block.header.gas_limit,
                    gas_used = %block.header.gas_used,
                    transactions_count = %block.body.transactions.len(),
                    "receive new block"
                );

                let event = BlockEvent::NewBlock {
                    peer_id,
                    block_number,
                    block_hash: block_msg.hash.to_string(),
                    transaction_count: block.body.transactions.len(),
                };

                if let Err(e) = self.event_sender.send(event) {
                    warn!("failed to send block event: {}", e);
                }

                if !block.body.transactions.is_empty() {
                    info!(
                        block_number = %block_number,
                        "block contains transactions count: {}",
                        block.body.transactions.len()
                    );
                }
            }
            NewBlockEvent::Hashes(hashes) => {
                info!(
                    peer_id = %peer_id,
                    hashes_count = %hashes.0.len(),
                    "receive block hashes list"
                );

                let block_numbers: Vec<u64> = hashes.0.iter().map(|h| h.number).collect();

                for hash_data in &hashes.0 {
                    info!(
                        peer_id = %peer_id,
                        block_hash = %hash_data.hash,
                        block_number = %hash_data.number,
                        "block hash"
                    );
                }

                let event = BlockEvent::NewBlockHashes {
                    peer_id,
                    block_numbers,
                };

                if let Err(e) = self.event_sender.send(event) {
                    warn!("failed to send block hashes event: {}", e);
                }
            }
        }
    }

    fn poll(&mut self, _cx: &mut Context<'_>) -> Poll<BlockImportEvent<reth_eth_wire::NewBlock>> {
        Poll::Pending
    }
}
