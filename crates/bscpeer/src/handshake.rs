use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::SinkExt;
use reth_eth_wire::{
    UnifiedStatus,
    errors::{EthHandshakeError, EthStreamError},
    handshake::{EthRlpxHandshake, EthereumEthHandshake, UnauthEth},
};
use reth_eth_wire_types::{DisconnectReason, EthVersion};
use reth_ethereum_forks::ForkFilter;
use std::{future::Future, pin::Pin};
use tokio::time::{Duration, timeout};
use tokio_stream::StreamExt;

use tracing::debug;

/// The message id for the upgrade status message, used in the BSC handshake.
const UPGRADE_STATUS_MESSAGE_ID: u8 = 0x0b;

/// UpdateStatus packet introduced in BSC to notify peers whether to broadcast transaction or not.
/// It is used during the p2p handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UpgradeStatus {
    /// Extension for support customized features for BSC.
    pub extension: UpgradeStatusExtension,
}

impl Encodable for UpgradeStatus {
    fn encode(&self, out: &mut dyn BufMut) {
        UPGRADE_STATUS_MESSAGE_ID.encode(out);
        self.extension.encode(out);
    }
}

impl Decodable for UpgradeStatus {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let message_id = u8::decode(buf)?;
        if message_id != UPGRADE_STATUS_MESSAGE_ID {
            return Err(alloy_rlp::Error::Custom("Invalid message ID"));
        }
        buf.advance(1);
        let extension = UpgradeStatusExtension::decode(buf)?;
        Ok(Self { extension })
    }
}

impl UpgradeStatus {
    /// Encode the upgrade status message into RLPx bytes.
    pub fn into_rlpx(self) -> Bytes {
        let mut out = BytesMut::new();
        self.encode(&mut out);
        out.freeze()
    }
}

/// The extension to define whether to enable or disable the flag.
/// This flag currently is ignored, and will be supported later.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UpgradeStatusExtension {
    // TODO: support disable_peer_tx_broadcast flag
    /// To notify a peer to disable the broadcast of transactions or not.
    pub disable_peer_tx_broadcast: bool,
}

#[derive(Debug, Default)]
/// The Binance Smart Chain (BSC) P2P handshake.
#[non_exhaustive]
pub struct BscHandshake;

impl BscHandshake {
    /// Negotiate the upgrade status message.
    pub async fn upgrade_status(
        unauth: &mut dyn UnauthEth,
        negotiated_status: UnifiedStatus,
    ) -> Result<UnifiedStatus, EthStreamError> {
        if negotiated_status.version > EthVersion::Eth66 {
            // Send upgrade status message allowing peer to broadcast transactions
            let upgrade_msg = UpgradeStatus {
                extension: UpgradeStatusExtension {
                    disable_peer_tx_broadcast: false,
                },
            };
            unauth.start_send_unpin(upgrade_msg.into_rlpx())?;

            // Receive peer's upgrade status response
            let their_msg = match unauth.next().await {
                Some(Ok(msg)) => msg,
                Some(Err(e)) => return Err(EthStreamError::from(e)),
                None => {
                    unauth
                        .disconnect(DisconnectReason::DisconnectRequested)
                        .await?;
                    return Err(EthStreamError::EthHandshakeError(
                        EthHandshakeError::NoResponse,
                    ));
                }
            };

            // Decode their response
            match UpgradeStatus::decode(&mut their_msg.as_ref()).map_err(|e| {
                debug!("Decode error in BSC handshake: msg={their_msg:x}");
                EthStreamError::InvalidMessage(e.into())
            }) {
                Ok(_) => {
                    // Successful handshake
                    return Ok(negotiated_status);
                }
                Err(_) => {
                    unauth.disconnect(DisconnectReason::ProtocolBreach).await?;
                    return Err(EthStreamError::EthHandshakeError(
                        EthHandshakeError::NonStatusMessageInHandshake,
                    ));
                }
            }
        }

        Ok(negotiated_status)
    }
}

impl EthRlpxHandshake for BscHandshake {
    fn handshake<'a>(
        &'a self,
        unauth: &'a mut dyn UnauthEth,
        status: UnifiedStatus,
        fork_filter: ForkFilter,
        timeout_limit: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<UnifiedStatus, EthStreamError>> + 'a + Send>> {
        Box::pin(async move {
            let fut = async {
                let negotiated_status = EthereumEthHandshake(unauth)
                    .eth_handshake(status, fork_filter)
                    .await?;
                Self::upgrade_status(unauth, negotiated_status).await
            };
            timeout(timeout_limit, fut)
                .await
                .map_err(|_| EthStreamError::StreamTimeout)?
        })
    }
}
