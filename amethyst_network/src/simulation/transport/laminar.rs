//! Network systems implementation backed by the Laminar network protocol.

use crate::simulation::{
    events::NetworkSimulationEvent,
    requirements::DeliveryRequirement,
    timing::*,
    transport::{
        TransportResource, NETWORK_POLL_SYSTEM_NAME, NETWORK_RECV_SYSTEM_NAME,
        NETWORK_SEND_SYSTEM_NAME, NETWORK_SIM_TIME_SYSTEM_NAME,
    },
};
use amethyst_core::{
    ecs::prelude::*,
    dispatcher::{DispatcherBuilder, Stage, SystemBundle},
    shrev::EventChannel,
};
use amethyst_error::Error;
pub use laminar::{Config as LaminarConfig, ErrorKind, Socket as LaminarSocket};
use laminar::{Packet, SocketEvent};

use bytes::Bytes;
use log::error;
use std::time::Instant;

/// Use this network bundle to add the laminar transport layer to your game.
pub struct LaminarNetworkBundle {
    socket: Option<LaminarSocket>,
}

impl LaminarNetworkBundle {
    pub fn new(socket: Option<LaminarSocket>) -> Self {
        Self { socket }
    }
}

impl SystemBundle for LaminarNetworkBundle {
    fn build(
        self,
        world: &mut World,
        resources: &mut Resources, 
        builder: &mut DispatcherBuilder<'_>,
    ) -> Result<(), Error> {
        builder.add_system(Stage::Begin, build_network_simulation_time_system);
        builder.add_system(Stage::Begin, build_laminar_network_send_system);
        builder.add_system(Stage::Begin, build_laminar_network_poll_system);
        builder.add_system(Stage::Begin, build_laminar_network_recv_system);

        resources.insert(LaminarSocketResource::new(self.socket));
        Ok(())
    }
}

pub fn build_laminar_network_send_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("LaminarNetworkSendSystem")
        .write_resource::<TransportResource>()
        .write_resource::<LaminarSocketResource>()
        .read_resource::<NetworkSimulationTime>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(
            move |_commands, world, (transport, socket, sim_time, event_channel), ()| {
                if let Some(socket) = socket.get_mut() {
                    let messages = transport.drain_messages_to_send(|_| sim_time.should_send_message_now());

                    for message in messages {
                        let packet = match message.delivery {
                            DeliveryRequirement::Unreliable => {
                                Packet::unreliable(message.destination, message.payload.to_vec())
                            }
                            DeliveryRequirement::UnreliableSequenced(stream_id) => {
                                Packet::unreliable_sequenced(
                                    message.destination,
                                    message.payload.to_vec(),
                                    stream_id,
                                )
                            }
                            DeliveryRequirement::Reliable => {
                                Packet::reliable_unordered(message.destination, message.payload.to_vec())
                            }
                            DeliveryRequirement::ReliableSequenced(stream_id) => {
                                Packet::reliable_sequenced(
                                    message.destination,
                                    message.payload.to_vec(),
                                    stream_id,
                                )
                            }
                            DeliveryRequirement::ReliableOrdered(stream_id) => Packet::reliable_ordered(
                                message.destination,
                                message.payload.to_vec(),
                                stream_id,
                            ),
                            DeliveryRequirement::Default => Packet::reliable_ordered(
                                message.destination,
                                message.payload.to_vec(),
                                None,
                            ),
                        };

                        match socket.send(packet) {
                            Err(ErrorKind::IOError(e)) => {
                                event_channel.single_write(NetworkSimulationEvent::SendError(e, message));
                            }
                            Err(e) => {
                                error!("Error sending message: {:?}", e);
                            }
                            Ok(_) => {}
                        }
                    }
                }
            }
        )
}

pub fn build_laminar_network_poll_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("LaminarNetworkPollSystem")
        .write_resource::<LaminarSocketResource>()
        .build(move |_commands, world, socket, ()| {
            if let Some(socket) = socket.get_mut() {
                socket.manual_poll(Instant::now());
            }
        })
}

pub fn build_laminar_network_recv_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("LaminarNetworkReceiveSystem")
        .write_resource::<LaminarSocketResource>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(move |_commands, world, (socket, event_channel), ()| {
            if let Some(socket) = socket.get_mut() {
                while let Some(event) = socket.recv() {
                    let event = match event {
                        SocketEvent::Packet(packet) => NetworkSimulationEvent::Message(
                            packet.addr(),
                            Bytes::copy_from_slice(packet.payload()),
                        ),
                        SocketEvent::Connect(addr) => NetworkSimulationEvent::Connect(addr),
                        SocketEvent::Timeout(addr) => NetworkSimulationEvent::Disconnect(addr),
                    };
                    event_channel.single_write(event);
                }
            }
        })
}

/// Resource that owns the Laminar socket.
pub struct LaminarSocketResource {
    socket: Option<LaminarSocket>,
}

impl Default for LaminarSocketResource {
    fn default() -> Self {
        Self { socket: None }
    }
}

impl LaminarSocketResource {
    /// Creates a new instance of the `UdpSocketResource`.
    pub fn new(socket: Option<LaminarSocket>) -> Self {
        Self { socket }
    }

    /// Returns a reference to the socket if there is one configured.
    pub fn get(&self) -> Option<&LaminarSocket> {
        self.socket.as_ref()
    }

    /// Returns a mutable reference to the socket if there is one configured.
    pub fn get_mut(&mut self) -> Option<&mut LaminarSocket> {
        self.socket.as_mut()
    }

    /// Sets the bound socket to the `LaminarSocketResource`.
    pub fn set_socket(&mut self, socket: LaminarSocket) {
        self.socket = Some(socket);
    }

    /// Drops the socket from the `LaminarSocketResource`.
    pub fn drop_socket(&mut self) {
        self.socket = None;
    }
}
