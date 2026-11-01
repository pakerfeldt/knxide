//! Device management services addressed to one device by its individual address.

use std::time::Duration;

use knxide_core::{Apdu, Cemi, IndividualAddress, LData, Priority, Tpdu};
use knxide_ip::tunnel::SendError;

use crate::client::{Client, ClientEvent};
use crate::error::Error;

/// How long to wait for the device to acknowledge the restart request before closing the
/// connection anyway. Spec 3/3/4: transport layer acknowledgement timeout, 3 s.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);

impl Client {
    /// Restart a device: a basic `A_Restart`, as ETS's "Restart device" does
    /// (DM_Restart, spec 3/5/2 §3.7.3).
    ///
    /// Opens a transport connection to `device`, sends the restart, and closes the connection.
    /// The device restarts its application and keeps its configuration. A master reset, which
    /// can erase the configuration, is deliberately not offered.
    ///
    /// Fails with [`Error::DeviceNotResponding`] if no device acknowledges `device` on the bus.
    pub async fn restart_device(&self, device: IndividualAddress) -> Result<(), Error> {
        let mut events = self.subscribe();
        self.send_to_device(device, Tpdu::Connect).await?;
        let restart = self
            .send_to_device(
                device,
                Tpdu::Connected {
                    sequence: 0,
                    apdu: Apdu::restart(),
                },
            )
            .await;
        if restart.is_ok() {
            // The device should acknowledge before restarting; many restart straight away.
            let acknowledged = tokio::time::timeout(ACK_TIMEOUT, async {
                loop {
                    match events.recv().await {
                        Ok(ClientEvent::Frame(ldata)) if ldata.source == device => {
                            if matches!(ldata.tpdu, Tpdu::Ack { sequence: 0 } | Tpdu::Disconnect) {
                                return;
                            }
                        }
                        Ok(ClientEvent::Closed)
                        | Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        _ => {}
                    }
                }
            })
            .await;
            if acknowledged.is_err() {
                tracing::debug!(%device, "no T_ACK for the restart request");
            }
        }
        // Close our side whatever happened; the device may already have dropped it.
        let _ = self.send_to_device(device, Tpdu::Disconnect).await;
        restart
    }

    /// Send a TPDU to one device, at system priority like other management traffic.
    async fn send_to_device(&self, device: IndividualAddress, tpdu: Tpdu) -> Result<(), Error> {
        let mut ldata = LData::new(device, tpdu);
        ldata.priority = Priority::System;
        match self.send(Cemi::LDataReq(ldata)).await {
            // An individually addressed frame that nobody acknowledges on the bus comes back
            // as a negative confirmation.
            Err(Error::Send(SendError::NegativeConfirmation)) => {
                Err(Error::DeviceNotResponding(device))
            }
            other => other,
        }
    }
}
