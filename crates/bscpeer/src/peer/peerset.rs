use reth_network_peers::PeerId;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Debug, Clone)]
pub struct BSCGatewayPeerSet {
    connected_peers: Arc<Mutex<Vec<PeerId>>>,
}

impl BSCGatewayPeerSet {
    pub fn new() -> Self {
        Self {
            connected_peers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn add_peer(&self, peer_id: PeerId) {
        let mut peers = self.connected_peers.lock().unwrap();

        // Add permission control logic here if needed

        if !peers.contains(&peer_id) {
            peers.push(peer_id);
            info!(%peer_id, "peerset add new peer");
        }
    }

    fn remove_peer(&self, peer_id: &PeerId) {
        let mut peers = self.connected_peers.lock().unwrap();
        peers.retain(|p| p != peer_id);
        info!(%peer_id, "peerset remove peer");
    }

    // get all peers
    pub fn get_all_peers(&self) -> Vec<PeerId> {
        self.connected_peers.lock().unwrap().clone()
    }

    // get peer count
    pub fn get_peer_count(&self) -> usize {
        self.connected_peers.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use reth_network_peers::AnyNode;

    use super::*;

    #[test]
    fn test_add_peer() {
        let peer_set = BSCGatewayPeerSet::new();
        let url = "enode://6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0";
        let node: AnyNode = url.parse().unwrap();
        let peer_id = node.peer_id();

        peer_set.add_peer(peer_id);
        assert_eq!(peer_set.get_all_peers().len(), 1);
        assert_eq!(peer_set.get_peer_count(), 1);

        // add same peer again
        peer_set.add_peer(peer_id);
        assert_eq!(peer_set.get_all_peers().len(), 1);

        // get all peers
        let all_peers = peer_set.get_all_peers();
        assert_eq!(all_peers.len(), 1);
        assert!(all_peers.contains(&peer_id));

        // get peer count
        assert_eq!(peer_set.get_peer_count(), 1);

        // remove peer
        peer_set.remove_peer(&peer_id);
        assert_eq!(peer_set.get_all_peers().len(), 0);
        assert_eq!(peer_set.get_peer_count(), 0);

        // remove same peer again
        peer_set.remove_peer(&peer_id);
        assert_eq!(peer_set.get_all_peers().len(), 0);
    }
}
