//! Network systems implementation backed by the TCP network protocol.

use crate::simulation::{
    events::NetworkSimulationEvent,
    message::Message,
    requirements::DeliveryRequirement,
    timing::*,
    transport::{
        TransportResource, NETWORK_RECV_SYSTEM_NAME, NETWORK_SEND_SYSTEM_NAME,
        NETWORK_SIM_TIME_SYSTEM_NAME,
    },
};
use amethyst_core::{
    ecs::prelude::*,
    dispatcher::{DispatcherBuilder, Stage, SystemBundle},
    shrev::EventChannel,
};
use amethyst_error::Error;
use bytes::Bytes;
use log::warn;
use std::{
    collections::HashMap,
    io::{self, Read as IORead, Write as IOWrite},
    net::{SocketAddr, TcpListener, TcpStream},
    ops::DerefMut,
};

const CONNECTION_LISTENER_SYSTEM_NAME: &str = "connection_listener";
const STREAM_MANAGEMENT_SYSTEM_NAME: &str = "stream_management";

/// Use this network bundle to add the TCP transport layer to your game.
pub struct TcpNetworkBundle {
    listener: Option<TcpListener>,
    recv_buffer_size_bytes: usize,
}

impl TcpNetworkBundle {
    pub fn new(listener: Option<TcpListener>, recv_buffer_size_bytes: usize) -> Self {
        Self {
            listener,
            recv_buffer_size_bytes,
        }
    }
}

impl SystemBundle for TcpNetworkBundle {
    fn build(
        self,
        world: &mut World,
        resources: &mut Resources,
        builder: &mut DispatcherBuilder<'_>,
    ) -> Result<(), Error> {
        // NetworkSimulationTime should run first
        // followed by TcpConnectionListenerSystem and TcpStreamManagementSystem
        // then TcpNetworkSendSystem and TcpNetworkRecvSystem

        builder.add_system(Stage::Begin, build_network_simulation_time_system);
        builder.add_system(Stage::Begin, build_tcp_connection_listener_system);
        builder.add_system(Stage::Begin, build_tcp_stream_management_system);
        builder.add_system(Stage::Begin, build_tcp_network_send_system);
        builder.add_system(Stage::Begin, build_tcp_network_recv_system);

        resources.insert(TcpNetworkResource::new(
            self.listener,
            self.recv_buffer_size_bytes,
        ));
        Ok(())
    }
}

/// System to manage the current active TCPStreams.
pub fn build_tcp_stream_management_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("TcpStreamManagementSystem")
        .write_resource::<TcpNetworkResource>()
        .read_resource::<TransportResource>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(
            move |_commands, world, (net, transport, event_channel), ()| {
                transport.get_messages().iter().for_each(|message| {
                    if !net.streams.contains_key(&message.destination) {
                        let s = match TcpStream::connect(message.destination) {
                            Ok(s) => s,
                            Err(e) => {
                                event_channel.single_write(NetworkSimulationEvent::ConnectionError(
                                    e,
                                    Some(message.destination),
                                ));
                                return;
                            }
                        };
                        s.set_nonblocking(true).expect("Setting non-blocking mode");
                        s.set_nodelay(true).expect("Setting nodelay");
                        net.streams.insert(message.destination, (true, s));
                    }
                });

                net.streams.retain(|addr, (active, _)| {
                    if !*active {
                        event_channel.single_write(NetworkSimulationEvent::Disconnect(*addr));
                    }
                    *active
                });
            }
        )

}

/// System to listen for incoming connections and cache them to the resource.
pub fn build_tcp_connection_listener_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("TcpConnectionListenerSystem")
        .write_resource::<TcpNetworkResource>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(move |_commands, world, (net, event_channel), ()| {
            let resource = net.deref_mut();
            if let Some(ref listener) = resource.listener {
                loop {
                    match listener.accept() {
                        Ok((stream, addr)) => {
                            stream
                                .set_nonblocking(true)
                                .expect("Setting nonblocking mode");
                            stream.set_nodelay(true).expect("Setting nodelay");
                            resource.streams.insert(addr, (true, stream));
                            event_channel.single_write(NetworkSimulationEvent::Connect(addr));
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            break;
                        }
                        Err(e) => {
                            event_channel
                                .single_write(NetworkSimulationEvent::ConnectionError(e, None));
                            break;
                        }
                    };
                }
            }
        })
}

/// System to send messages to a particular open `TcpStream`.
pub fn build_tcp_network_send_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("TcpNetworkSendSystem")
        .write_resource::<TransportResource>()
        .write_resource::<TcpNetworkResource>()
        .read_resource::<NetworkSimulationTime>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(move |_commands, world, (transport, net, sim_time, channel), ()| {
            let messages = transport.drain_messages_to_send(|_| sim_time.should_send_message_now());
            for message in messages {
                match message.delivery {
                    DeliveryRequirement::ReliableOrdered(Some(_)) => {
                        warn!("Streams are not supported by TCP and will be ignored.");
                        write_message(message, net, channel);
                    }
                    DeliveryRequirement::ReliableOrdered(_) | DeliveryRequirement::Default => {
                        write_message(message, net, channel);
                    }
                    delivery => panic!(
                        "{:?} is unsupported. TCP only supports ReliableOrdered by design.",
                        delivery
                    ),
                }
            }
        })
}

fn write_message(
    message: Message,
    net: &mut TcpNetworkResource,
    channel: &mut EventChannel<NetworkSimulationEvent>,
) {
    if let Some((_, stream)) = net.get_stream(message.destination) {
        if let Err(e) = stream.write(&message.payload) {
            channel.single_write(NetworkSimulationEvent::SendError(e, message));
        }
    }
}

/// System to receive messages from all open `TcpStream`s.
pub fn build_tcp_network_recv_system(_world: &mut World, _res: &mut Resources) -> Box<dyn Schedulable> {
    SystemBuilder::<()>::new("TcpNetworkReceiveSystem")
        .write_resource::<TcpNetworkResource>()
        .write_resource::<EventChannel<NetworkSimulationEvent>>()
        .build(move |_commands, world, (net, event_channel), ()| {
            let resource = net.deref_mut();
            for (_, (active, stream)) in resource.streams.iter_mut() {
                // If we can't get a peer_addr, there is likely something pretty wrong with the
                // connection so we'll mark it inactive.
                let peer_addr = match stream.peer_addr() {
                    Ok(addr) => addr,
                    Err(e) => {
                        warn!("Encountered an error getting peer_addr: {:?}", e);
                        *active = false;
                        continue;
                    }
                };

                loop {
                    match stream.read(&mut resource.recv_buffer) {
                        Ok(recv_len) => {
                            if recv_len > 0 {
                                let event = NetworkSimulationEvent::Message(
                                    peer_addr,
                                    Bytes::copy_from_slice(&resource.recv_buffer[..recv_len]),
                                );
                                event_channel.single_write(event);
                            } else {
                                *active = false;
                                break;
                            }
                        }
                        Err(e) => {
                            match e.kind() {
                                io::ErrorKind::ConnectionReset => {
                                    *active = false;
                                }
                                io::ErrorKind::WouldBlock => {}
                                _ => {
                                    event_channel.single_write(NetworkSimulationEvent::RecvError(e));
                                }
                            }
                            break;
                        }
                    }
                }
            }
        })
}

pub struct TcpNetworkResource {
    listener: Option<TcpListener>,
    streams: HashMap<SocketAddr, (bool, TcpStream)>,
    recv_buffer: Vec<u8>,
}

impl TcpNetworkResource {
    pub fn new(listener: Option<TcpListener>, recv_buffer_size_bytes: usize) -> Self {
        Self {
            listener,
            streams: HashMap::new(),
            recv_buffer: vec![0; recv_buffer_size_bytes],
        }
    }

    /// Returns an immutable reference to the listener if there is one configured.
    pub fn get(&self) -> Option<&TcpListener> {
        self.listener.as_ref()
    }

    /// Returns a mutable reference to the listener if there is one configured.
    pub fn get_mut(&mut self) -> Option<&mut TcpListener> {
        self.listener.as_mut()
    }

    /// Sets the bound listener to the `TcpNetworkResource`.
    pub fn set_listener(&mut self, listener: TcpListener) {
        self.listener = Some(listener);
    }

    /// Drops the listener from the `TcpNetworkResource`.
    pub fn drop_listener(&mut self) {
        self.listener = None;
    }

    /// Returns a tuple of an active TcpStream and whether ot not that stream is active
    pub fn get_stream(&mut self, addr: SocketAddr) -> Option<&mut (bool, TcpStream)> {
        self.streams.get_mut(&addr)
    }

    /// Drops the stream with the given `SocketAddr`. This will be called when a peer seems to have
    /// been disconnected
    pub fn drop_stream(&mut self, addr: SocketAddr) -> Option<(bool, TcpStream)> {
        self.streams.remove(&addr)
    }
}

impl Default for TcpNetworkResource {
    fn default() -> Self {
        Self {
            listener: None,
            streams: HashMap::new(),
            recv_buffer: Vec::new(),
        }
    }
}
