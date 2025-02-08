use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crossbeam_channel::{select, select_biased, Receiver, Sender};
use dronegowski_utils::functions::{
    assembler, deserialize_message, fragment_message, generate_unique_id,
};
use dronegowski_utils::hosts::{
    ClientCommand, ClientEvent, ClientMessages, ClientType, ServerMessages, TestMessage,
};
use serde::Serialize;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{FloodRequest, FloodResponse, Fragment, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::Ack;

#[derive(Debug)]
pub struct DronegowskiClient {
    pub id: NodeId,
    sim_controller_send: Sender<ClientEvent>,
    sim_controller_recv: Receiver<ClientCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
    pub client_type: ClientType,
    message_storage: HashMap<(usize, NodeId), (Vec<u8>, Vec<bool>)>,
    topology: HashSet<(NodeId, NodeId)>,
    node_types: HashMap<NodeId, NodeType>,
}

impl DronegowskiClient {
    pub fn new(
        id: NodeId,
        sim_controller_send: Sender<ClientEvent>,
        sim_controller_recv: Receiver<ClientCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        client_type: ClientType,
    ) -> Self {
        let mut client = Self {
            id,
            sim_controller_send,
            sim_controller_recv,
            packet_recv,
            packet_send,
            client_type,
            message_storage: HashMap::new(),
            topology: HashSet::new(),
            node_types: HashMap::new(),
        };

        client.server_discovery();
        client
    }

    pub fn run(&mut self) {
        loop {
            select_biased! {
                recv(self.packet_recv) -> packet_res => {
                    if let Ok(packet) = packet_res {
                        self.handle_packet(packet);
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    if let Ok(command) = command_res {
                        self.handle_client_command(command);
                    }
                }
            }
        }
    }

    fn handle_client_command(&mut self, command: ClientCommand) {
        log::info!("Client {}: Received {:?}", self.id, command);
        match command {
            ClientCommand::RemoveSender(node_id) => {
                self.remove_neighbor(&node_id);
                self.server_discovery();
            }
            ClientCommand::AddSender(node_id, packet_sender) => {
                self.add_neighbor(node_id, packet_sender);
                self.server_discovery();
            }
            ClientCommand::ServerType(node_id) => self.request_server_type(&node_id),
            ClientCommand::FilesList(node_id) => self.request_file_list(&node_id),
            ClientCommand::File(node_id, file_id) => self.request_file(&node_id, file_id),
            ClientCommand::Media(node_id, media_id) => self.request_media(&node_id, media_id),
            ClientCommand::ClientList(node_id) => self.request_client_list(&node_id),
            ClientCommand::RegistrationToChat(node_id) => self.register_with_server(&node_id),
            ClientCommand::MessageFor(node_id, client_id, message) => {
                self.send_message(&node_id, client_id, message)
            }
            ClientCommand::RequestNetworkDiscovery => self.server_discovery(),
        }
    }

    fn handle_packet(&mut self, packet: Packet) {
        log::info!("Client {}: Received {:?}", self.id, packet);

        match packet.pack_type {
            PacketType::MsgFragment(_) => self.handle_message_fragment(packet),
            PacketType::FloodResponse(flood_response) => {
                log::info!(
                    "Client {}: Received FloodResponse: {:?}",
                    self.id,
                    flood_response
                );
                self.update_graph(flood_response.path_trace);
            }
            PacketType::FloodRequest(_) => self.handle_flood_request(packet),
            Ack(session_id) => {
                log::info!("Client {}: Received Ack for session: {}", self.id, session_id);
            }
            PacketType::Nack(_) => {}
        }
    }

    fn handle_message_fragment(&mut self, packet: Packet) {
        let fragment = match packet.pack_type {
            PacketType::MsgFragment(f) => f,
            _ => return,
        };

        let src_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                log::warn!("Client {}: MsgFragment without source", self.id);
                return;
            }
        };

        log::info!(
            "Client {}: Received MsgFragment from: {}, Session: {}, Index: {}, Total: {}",
            self.id,
            src_id,
            packet.session_id,
            fragment.fragment_index,
            fragment.total_n_fragments
        );

        let reassembled_data = {
            let key = (packet.session_id as usize, src_id);
            let (message_data, fragments_received) = self
                .message_storage
                .entry(key)
                .or_insert_with(|| {
                    log::info!(
                        "Client {}: Initializing storage for session {} from {}",
                        self.id,
                        packet.session_id,
                        src_id
                    );
                    (
                        Vec::with_capacity((fragment.total_n_fragments * 128) as usize),
                        vec![false; fragment.total_n_fragments as usize],
                    )
                });

            assembler(message_data, &fragment);
            if (fragment.fragment_index as usize) < fragments_received.len() {
                fragments_received[fragment.fragment_index as usize] = true;
            }

            let all_fragments_received = fragments_received.iter().all(|&received| received);

            let percentage = (fragments_received.iter().filter(|&&r| r).count() * 100)
                / fragment.total_n_fragments as usize;
            log::info!(
                "Client {}: Fragment {}/{} for session {} from {}. {}% complete.",
                self.id,
                fragment.fragment_index + 1,
                fragment.total_n_fragments,
                packet.session_id,
                src_id,
                percentage
            );
            if all_fragments_received {
                Some((packet.session_id, message_data.clone()))
            } else {
                None
            }
        };

        if let Some((session_id, message_data)) = reassembled_data {
            self.process_reassembled_message(session_id, src_id, &message_data);
            self.message_storage.remove(&(session_id as usize, src_id));
        }
    }

    fn process_reassembled_message(&mut self, session_id: u64, src_id: NodeId, message_data: &[u8]) {
        match deserialize_message::<TestMessage>(message_data) {
            Ok(TestMessage::WebServerMessages(client_message)) => {
                self.handle_server_message(src_id, client_message);
            }
            Ok(deserialized_message) => {
                log::info!(
                    "Client {}: Message from session {} from {} fully reassembled: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    deserialized_message
                );
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MessageReceived(deserialized_message));
            }
            Err(e) => {
                log::error!(
                    "Client {}: Error deserializing session {} from {}: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    e
                );
            }
        }
    }

    fn handle_server_message(&mut self, src_id: NodeId, client_message: ClientMessages) {
        match client_message {
            ClientMessages::ServerMessages(server_message) => match server_message {
                ServerMessages::ServerType(server_type) => {
                    log::info!("Client {}: Received ServerType: {:?}", self.id, server_type);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ServerTypeReceived(src_id, server_type));
                }
                ServerMessages::ClientList(clients) => {
                    log::info!("Client {}: Received ClientList: {:?}", self.id, clients);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ClientListReceived(src_id, clients));
                }
                ServerMessages::FilesList(files) => {
                    log::info!("Client {}: Received FilesList: {:?}", self.id, files);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FilesListReceived(src_id, files));
                }
                ServerMessages::File(file_data) => {
                    log::info!(
                        "Client {}: Received File data (size: {} bytes)",
                        self.id,
                        file_data.text.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FileReceived(src_id, file_data.text));
                }
                ServerMessages::Media(media_data) => {
                    log::info!(
                        "Client {}: Received Media data (size: {} bytes)",
                        self.id,
                        media_data.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::MediaReceived(src_id, media_data));
                }
                ServerMessages::MessageFrom(from_id, message) => {
                    log::info!(
                        "Client {}: Received MessageFrom: {} from {}",
                        self.id,
                        message,
                        from_id
                    );
                    let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived(
                        src_id, from_id, message,
                    ));
                }
                ServerMessages::RegistrationOk => {
                    log::info!("Client {}: Received RegistrationOk", self.id);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationOk(src_id));
                }
                ServerMessages::RegistrationError(error) => {
                    log::info!(
                        "Client {}: Received RegistrationError, cause: {}",
                        self.id,
                        error
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationError(src_id));
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn switch_client_type(&mut self) {
        log::info!("Client Type Switched");
        self.client_type = match self.client_type {
            ClientType::ChatClients => ClientType::WebBrowsers,
            ClientType::WebBrowsers => ClientType::ChatClients,
        };
    }

    pub fn server_discovery(&self) {
        let flood_request = FloodRequest {
            flood_id: generate_unique_id(),
            initiator_id: self.id,
            path_trace: vec![(self.id, NodeType::Client)],
        };

        for (&node_id, sender) in &self.packet_send {
            log::info!("Sending FloodRequest to node {}", node_id);
            let packet = Packet {
                pack_type: PacketType::FloodRequest(flood_request.clone()),
                routing_header: SourceRoutingHeader {
                    hop_index: 0,
                    hops: vec![self.id, node_id],
                },
                session_id: flood_request.flood_id,
            };
            self.send_packet_and_notify(packet, node_id);
        }
    }

    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        log::info!("Updating graph with: {:?}", path_trace);
        for i in 0..path_trace.len() - 1 {
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b));
            self.topology.insert((node_b, node_a));
        }

        for (node_id, node_type) in path_trace {
            self.node_types.insert(node_id, node_type);
        }
    }

    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        log::info!("Client {}: Computing route to {}", self.id, target_server);
        log::info!("Client {}: Current topology: {:?}", self.id, self.topology);

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut predecessors: HashMap<NodeId, NodeId> = HashMap::new(); // Store predecessors for path reconstruction

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current_node) = queue.pop_front() {
            log::debug!("Client {}:  Current node in BFS: {}", self.id, current_node);

            if current_node == *target_server {
                log::debug!("Client {}: Target server {} found!", self.id, target_server);
                let mut path = Vec::new();
                let mut current = *target_server;
                while let Some(&prev) = predecessors.get(&current) {
                    path.push(current);
                    current = prev;
                }
                path.push(self.id); // Add the starting node (client itself)
                path.reverse(); // Reverse the path to get the correct order
                log::info!("Client {}:  Found path: {:?}", self.id, path);
                return Some(path);
            }

            // Iterate through neighbors based on the *bidirectional* topology
            for &(node_a, node_b) in &self.topology {
                // Check neighbors in both directions
                if node_a == current_node && !visited.contains(&node_b) {
                    log::debug!("Client {}:  Exploring neighbor: {} of {}", self.id, node_b, node_a);
                    visited.insert(node_b);
                    queue.push_back(node_b);
                    predecessors.insert(node_b, node_a); // Store the predecessor
                } else if node_b == current_node && !visited.contains(&node_a) {
                    log::debug!("Client {}:  Exploring neighbor: {} of {}", self.id, node_a, node_b);
                    visited.insert(node_a);
                    queue.push_back(node_a);
                    predecessors.insert(node_a, node_b);  // Store the predecessor
                }
            }
        }

        log::warn!("Client {}: No route found to {}", self.id, target_server);
        None
    }

    fn send_client_message_to_server(
        &self,
        server_id: &NodeId,
        client_message: ClientMessages,
    ) {
        let message = TestMessage::WebServerMessages(client_message);
        self.send_message_to_node(server_id, message);
    }

    pub fn register_with_server(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::RegistrationToChat);
    }

    pub fn request_client_list(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::ClientList);
    }

    pub fn send_message(&self, server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        self.send_client_message_to_server(
            server_id,
            ClientMessages::MessageFor(target_id, message_to_client),
        );
    }

    pub fn request_file_list(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::FilesList);
    }

    pub fn request_file(&self, server_id: &NodeId, file_id: u64) {
        self.send_client_message_to_server(server_id, ClientMessages::File(file_id));
    }

    pub fn request_media(&self, server_id: &NodeId, file_id: u64) {
        self.send_client_message_to_server(server_id, ClientMessages::Media(file_id));
    }

    pub fn request_server_type(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::ServerType);
    }

    fn send_message_to_node(&self, target_id: &NodeId, message: TestMessage) {
        if let Some(path) = self.compute_route(target_id) {
            log::info!("Sto per mandare il messaggio, porco dio");
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            let fragments = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if let (Some(next_hop), true) = (path.get(1), path.len() > 1) {
                if let Some(sender) = self.packet_send.get(next_hop) {
                    for packet in fragments {
                        log::info!("self.send_packet_and_notify(packet, *next_hop);");
                        self.send_packet_and_notify(packet, *next_hop);
                    }
                } else {
                    log::error!("Client {}: No sender for next hop {}", self.id, next_hop);
                }
            } else {
                log::error!("Client {}: Invalid route to {}", self.id, target_id);
            }
        } else {
            log::warn!("Client {}: No route to {}", self.id, target_id);
        }
    }

    fn send_packet_and_notify(&self, packet: Packet, recipient_id: NodeId) {
        if let Some(sender) = self.packet_send.get(&recipient_id) {
            if let Err(e) = sender.send(packet.clone()) {
                log::error!(
                    "Client {}: Failed to send packet to {}: {:?}",
                    self.id,
                    recipient_id,
                    e
                );
            } else {
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::PacketSent(packet));
            }
        } else {
            log::error!("Client {}: No sender for node {}", self.id, recipient_id);
        }
    }

    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        if self.packet_send.insert(node_id, sender).is_some() {
            log::warn!("Client {}: Replaced existing sender for node {}", self.id, node_id);
        }
    }

    fn remove_neighbor(&mut self, node_id: &NodeId) {
        if self.packet_send.remove(node_id).is_none() {
            log::warn!("Client {}: Node {} was not a neighbor.", self.id, node_id);
        }
    }

    fn handle_flood_request(&mut self, packet: Packet) {
        let flood_request = match packet.pack_type {
            PacketType::FloodRequest(req) => req,
            _ => return,
        };

        log::info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request);

        let source_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                log::warn!("Client {}: FloodRequest without source", self.id);
                return;
            }
        };

        self.update_graph(flood_request.path_trace.clone());

        let mut response_path_trace = flood_request.path_trace.clone();
        response_path_trace.push((self.id, NodeType::Client));

        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: response_path_trace,
        };

        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: flood_request.path_trace.iter().rev().map(|(id, _)| *id).collect(),
            },
            session_id: packet.session_id,
        };

        self.send_packet_and_notify(response_packet, source_id);
    }

    fn send_message_with_timeout(
        &self,
        packet: Packet,
        recipient_id: NodeId,
        timeout: Duration,
    ) -> Result<(), ()> {
        match self.packet_send.get(&recipient_id) {
            Some(sender) => match sender.send_timeout(packet.clone(), timeout) {
                Ok(()) => {
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::PacketSent(packet));
                    Ok(())
                }
                Err(_) => {
                    log::warn!(
                        "Client {}: Timeout sending packet to {}",
                        self.id,
                        recipient_id
                    );
                    Err(())
                }
            },
            None => {
                log::warn!("Client {}: No sender for node {}", self.id, recipient_id);
                Err(())
            }
        }
    }
}