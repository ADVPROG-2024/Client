use std::collections::{HashMap, HashSet, VecDeque};

use crossbeam_channel::{select_biased, Receiver, Sender};
use dronegowski_utils::functions::{
    assembler, fragment_message, generate_unique_id,
};
use dronegowski_utils::hosts::{
    ClientCommand, ClientEvent, ClientMessages, ClientType, ServerMessages, TestMessage,
};
use log::{debug, error, info, warn};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{FloodRequest, FloodResponse, Nack, NackType, NodeType, Packet, PacketType};

/// `DronegowskiClient` represents a client within the simulation.
/// It manages communication with the simulator, sending and receiving packets,
/// and the client-specific logic (chat or web browsing).
#[derive(Debug)]
pub struct DronegowskiClient {
    /// Unique identifier of the client.
    pub id: NodeId,
    /// Channel to send events to the simulation controller.
    pub sim_controller_send: Sender<ClientEvent>,
    /// Channel to receive commands from the simulation controller.
    pub sim_controller_recv: Receiver<ClientCommand>,
    /// Channel to receive packets from the network.
    pub packet_recv: Receiver<Packet>,
    /// Map associating each node with a channel to send packets to it.
    pub packet_send: HashMap<NodeId, Sender<Packet>>,
    /// Type of client (ChatClients or WebBrowsers).
    pub client_type: ClientType,
    /// Map that stores incoming message fragments for reassembly.
    /// The key is a tuple (session_id, sender), the value is a tuple (message data, vector of booleans indicating which fragments have arrived).
    pub message_storage: HashMap<(usize, NodeId), (Vec<u8>, Vec<bool>)>,
    /// Set of tuples (NodeId, NodeId) representing the network topology as seen by the client.
    pub topology: HashSet<(NodeId, NodeId)>,
    /// Map associating each node with its type (Client, Server, Intermediate).
    pub node_types: HashMap<NodeId, NodeType>,
    // MESSAGE STORAGE
    pending_messages: HashMap<u64, Vec<Packet>>,
    acked_fragments: HashMap<u64, HashSet<u64>>, // Nuovo campo per tracciare i frammenti confermati
    // MAP THAT KEEPS COUNT OF NACK OF TYPE DROPPED RECEIVED FOR A CERTAIN FRAGMENT
    nack_counter: HashMap<(u64, u64, NodeId), u8>,
    excluded_nodes: HashSet<NodeId>,
}

impl DronegowskiClient {
    /// Creates a new `DronegowskiClient`.
    ///
    /// # Arguments
    ///
    /// * `id`: The ID of the client.
    /// * `sim_controller_send`: The channel to send events to the simulation controller.
    /// * `sim_controller_recv`: The channel to receive commands from the simulation controller.
    /// * `packet_recv`: The channel to receive packets.
    /// * `packet_send`: A map of channels to send packets to other nodes.
    /// * `client_type`: The type of client.
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
            pending_messages: HashMap::new(),
            nack_counter: HashMap::new(),
            excluded_nodes: HashSet::new(),
            acked_fragments: HashMap::new(),
        };

        // Performs initial server discovery.
        client.server_discovery();
        client
    }

    /// Starts the main loop of the client.
    ///
    /// The client listens on two channels:
    /// - `packet_recv`: for receiving packets.
    /// - `sim_controller_recv`: for receiving commands from the simulator.
    ///
    /// Uses `select_biased` to prioritize packet reception.
    pub fn run(&mut self) {
        loop {
            select_biased! {
                recv(self.packet_recv) -> packet_res => {
                    if let Ok(packet) = packet_res {
                        self.handle_packet(packet);
                    } else {
                        error!("Client {}: Error receiving packet", self.id);
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    if let Ok(command) = command_res {
                        self.handle_client_command(command);
                    } else {
                        error!("Client {}: Error receiving command from simulator", self.id);
                    }
                }
            }
        }
    }

    /// Handles a command received from the simulation controller.
    fn handle_client_command(&mut self, command: ClientCommand) {
        info!("Client {}: Received command from simulator: {:?}", self.id, command);
        match command {
            ClientCommand::RemoveSender(node_id) => {
                // Removes a neighbor and re-executes server discovery.
                self.remove_neighbor(&node_id);
                self.server_discovery();
            }
            ClientCommand::AddSender(node_id, packet_sender) => {
                // Adds a neighbor and re-executes server discovery.
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

    /// Handles a received packet.
    fn handle_packet(&mut self, packet: Packet) {
        info!("Client {}: Received packet: {:?}", self.id, packet);

        match packet.pack_type {
            PacketType::MsgFragment(_) => self.handle_message_fragment(packet),
            PacketType::FloodResponse(flood_response) => {
                info!(
                    "Client {}: Received FloodResponse: {:?}",
                    self.id,
                    flood_response
                );
                self.update_graph(flood_response.path_trace);
            }
            PacketType::FloodRequest(_) => self.handle_flood_request(packet),
            PacketType::Ack(ack) => {
                info!("Client {}: Received Ack for fragment {}", self.id, ack.fragment_index);
                let session_id = packet.session_id;
                let fragment_index = ack.fragment_index;

                // Rimuove le entry correlate dal nack_counter
                self.nack_counter.retain(|(f_idx, s_id, _), _| !(*f_idx == fragment_index && *s_id == session_id));

                // Aggiorna acked_fragments
                let acked = self.acked_fragments.entry(session_id).or_default();
                acked.insert(fragment_index);

                // Verifica se tutti i frammenti sono stati confermati
                if let Some(fragments) = self.pending_messages.get(&session_id) {
                    let total_fragments = fragments.len() as u64;
                    if acked.len() as u64 == total_fragments {
                        self.pending_messages.remove(&session_id);
                        self.acked_fragments.remove(&session_id);
                        info!("Client {}: All fragments for session {} have been acknowledged", self.id, session_id);
                    }
                }
            }
            PacketType::Nack(ref nack) => {
                // Nack packets are not handled at the moment. It might be necessary to implement them for error handling.
                info!("Client {}: Received Nack (unhandled)", self.id);
                let drop_drone = packet.clone().routing_header.hops[0];
                // NACK HANDLING METHOD
                self.handle_nack(nack.clone(), packet.session_id, drop_drone);
            }
        }
    }

    // Modifies the handle_nack function as follows
    fn handle_nack(&mut self, nack: Nack, session_id: u64, id_drop_drone: NodeId) {
        let key = (nack.fragment_index, session_id, id_drop_drone);

        // Uses Entry to correctly handle counter initialization
        let counter = self.nack_counter.entry(key).or_insert(0);
        *counter += 1;

        match nack.nack_type {
            NackType::Dropped => {
                if *counter > 5 {
                    info!("Client {}: Too many NACKs for fragment {}. Calculating alternative path", self.id, nack.fragment_index);

                    // Add the problematic node to excluded nodes
                    self.excluded_nodes.insert(id_drop_drone);

                    // Reconstruct the packet with a new path
                    if let Some(fragments) = self.pending_messages.get(&session_id) {
                        if let Some(packet) = fragments.get(nack.fragment_index as usize) {
                            if let Some(target_server) = packet.routing_header.hops.last() {
                                if let Some(new_path) = self.compute_route_excluding(target_server) {
                                    let mut new_packet = packet.clone();
                                    new_packet.routing_header.hops = new_path;
                                    new_packet.routing_header.hop_index = 1;

                                    if let Some(next_hop) = new_packet.routing_header.hops.get(1) {
                                        info!("Client {}: Resending fragment {} via new path: {:?}",
                                        self.id, nack.fragment_index, new_packet.routing_header.hops);
                                        self.send_packet_and_notify(new_packet.clone(), *next_hop); // Cloned here to fix borrow error

                                        // Reset the counter after rerouting
                                        self.nack_counter.remove(&key);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    warn!("Client {}: Unable to find alternative path", self.id);
                } else {
                    // Standard resend
                    if let Some(fragments) = self.pending_messages.get(&session_id) {
                        if let Some(packet) = fragments.get(nack.fragment_index as usize) {
                            info!("Client {}: Attempt {} for fragment {}",
                            self.id, counter, nack.fragment_index);
                            self.send_packet_and_notify(packet.clone(), packet.routing_header.hops[1]);
                        }
                    }
                }
            }
            _ => {
                // Handling other NACK types
                self.server_discovery();
                if let Some(fragments) = self.pending_messages.get(&session_id) {
                    if let Some(packet) = fragments.get(nack.fragment_index as usize) {
                        self.send_packet_and_notify(packet.clone(), packet.routing_header.hops[1]);
                    }
                }
            }
        }
    }

    // New function for calculating paths excluding nodes
    fn compute_route_excluding(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut predecessors = HashMap::new();

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current_node) = queue.pop_front() {
            if current_node == *target_server {
                let mut path = Vec::new();
                let mut current = *target_server;
                while let Some(prev) = predecessors.get(&current) {
                    path.push(current);
                    current = *prev;
                }
                path.push(self.id);
                path.reverse();
                return Some(path);
            }

            // Iterate over neighbors excluding problematic nodes
            for &(a, b) in &self.topology {
                if a == current_node && !self.excluded_nodes.contains(&b) && !visited.contains(&b) {
                    visited.insert(b);
                    queue.push_back(b);
                    predecessors.insert(b, a);
                } else if b == current_node && !self.excluded_nodes.contains(&a) && !visited.contains(&a) {
                    visited.insert(a);
                    queue.push_back(a);
                    predecessors.insert(a, b);
                }
            }
        }
        None
    }


    /// Handles a received message fragment.
    fn handle_message_fragment(&mut self, packet: Packet) {
        let fragment = match packet.pack_type {
            PacketType::MsgFragment(f) => f,
            _ => {
                // Should never happen, as this function is only called for MsgFragment.
                error!("Client {}: handle_message_fragment called with a non-MsgFragment packet type", self.id);
                return;
            }
        };

        let src_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                warn!("Client {}: MsgFragment without sender", self.id);
                return;
            }
        };

        info!(
            "Client {}: Received MsgFragment from: {}, Session: {}, Index: {}, Total: {}",
            self.id,
            src_id,
            packet.session_id,
            fragment.fragment_index,
            fragment.total_n_fragments
        );

        // Initialize ack_packet and next_hop outside the reassembled_data block
        let mut ack_packet_option: Option<Packet> = None;
        let mut next_hop_option: Option<NodeId> = None;

        // Logic for reassembling fragments.
        let reassembled_data = {
            let key = (packet.session_id as usize, src_id);
            // Gets or inserts a new entry in the `message_storage` map.
            let (message_data, fragments_received) = self
                .message_storage
                .entry(key)
                .or_insert_with(|| {
                    info!(
                        "Client {}: Initializing storage for session {} from {}",
                        self.id,
                        packet.session_id,
                        src_id
                    );
                    // Initializes the vector for message data and the vector to track received fragments.
                    (
                        Vec::with_capacity((fragment.total_n_fragments * 128) as usize),
                        vec![false; fragment.total_n_fragments as usize],
                    )
                });

            // Assembles the current fragment.
            assembler(message_data, &fragment);
            // Marks the fragment as received.
            if (fragment.fragment_index as usize) < fragments_received.len() {
                fragments_received[fragment.fragment_index as usize] = true;
            } else {
                error!("Client {}: Fragment index {} out of bounds for session {} from {}", self.id, fragment.fragment_index, packet.session_id, src_id);
                return;
            }

            // Invia un Ack al mittente per confermare la ricezione del pacchetto/frammento
            let reversed_hops: Vec<NodeId> = packet.routing_header.hops.iter().rev().cloned().collect();
            let ack_routing_header = SourceRoutingHeader {
                hop_index: 1,
                hops: reversed_hops,
            };

            let ack_packet = Packet::new_ack(
                ack_routing_header,
                packet.session_id,
                fragment.fragment_index,
            );

            if let Some(next_hop) = ack_packet.routing_header.hops.get(1).cloned() {
                info!("Client {}: Sending Ack for fragment {} to {}", self.id, fragment.fragment_index, next_hop);
                // Store ack_packet and next_hop for sending after mutable borrow ends
                ack_packet_option = Some(ack_packet);
                next_hop_option = Some(next_hop);
            } else {
                warn!("Client {}: No valid path to send Ack for fragment {}", self.id, fragment.fragment_index);
            }

            // Checks if all fragments have been received.
            let all_fragments_received = fragments_received.iter().all(|&received| received);


            let percentage = (fragments_received.iter().filter(|&&r| r).count() * 100) / fragment.total_n_fragments as usize;
            info!(
                "Client {}: Fragment {}/{} for session {} from {}. {}% complete.",
                self.id,
                fragment.fragment_index + 1,
                fragment.total_n_fragments,
                packet.session_id,
                src_id,
                percentage
            );


            if all_fragments_received {
                // If all fragments have been received, returns the message data.
                Some((packet.session_id, message_data.clone()))
            } else {
                // Otherwise, returns None.
                None
            }
        };

        // Send Ack packet after the mutable borrow of message_storage has ended
        if let (Some(ack_packet), Some(next_hop)) = (ack_packet_option, next_hop_option) {
            self.send_packet_and_notify(ack_packet, next_hop);
        }


        // If the message has been reassembled, processes it.
        if let Some((session_id, message_data)) = reassembled_data {
            self.process_reassembled_message(session_id, src_id, &message_data);
            // Removes the entry from the `message_storage` map.
            self.message_storage.remove(&(session_id as usize, src_id));
            info!("Client {}: Message from session {} from {} removed from storage", self.id, session_id, src_id);
        }
    }

    /// Processes a reassembled message.
    fn process_reassembled_message(&mut self, session_id: u64, src_id: NodeId, message_data: &[u8]) {
        // Deserializes the message.
        match bincode::deserialize(message_data) {
            Ok(TestMessage::WebClientMessages(server_message)) => {
                // If the message is a web server message, handles it.
                self.handle_server_message(src_id, server_message);
            }
            Ok(deserialized_message) => {
                info!(
                    "Client {}: Message from session {} from {} completely reassembled: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    deserialized_message
                );
                // Sends the received message to the simulation controller.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MessageReceived(deserialized_message));
            }
            Err(e) => {
                error!(
                    "Client {}: Error deserializing session {} from {}: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    e
                );
            }
        }
    }

    /// Handles a message received from a server.
    fn handle_server_message(&mut self, src_id: NodeId, server_message: ServerMessages) {

        match server_message {
            ServerMessages::ServerType(server_type) => {
                info!("Client {}: Received ServerType: {:?}", self.id, server_type);
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::ServerTypeReceived(self.id, src_id, server_type));
            }
            ServerMessages::ClientList(clients) => {
                info!("Client {}: Received ClientList: {:?}", self.id, clients);
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::ClientListReceived(self.id, src_id, clients));
            }
            ServerMessages::FilesList(files) => {
                info!("Client {}: Received FilesList: {:?}", self.id, files);
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::FilesListReceived(self.id, src_id, files));
            }
            ServerMessages::File(file_data) => {
                info!(
                    "Client {}: Received file data (size: {} bytes)",
                    self.id,
                    file_data.text.len()
                );
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::FileReceived(self.id, src_id, file_data.text));
            }
            ServerMessages::Media(media_data) => {
                info!(
                    "Client {}: Received media data (size: {} bytes)",
                    self.id,
                    media_data.len()
                );
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MediaReceived(self.id, src_id, media_data));
            }
            ServerMessages::MessageFrom(from_id, message) => {
                info!(
                    "Client {}: Received MessageFrom: {} from {}",
                    self.id,
                    message,
                    from_id
                );
                let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived(
                    self.id, src_id, from_id, message,
                ));
            }
            ServerMessages::RegistrationOk => {
                info!("Client {}: Received RegistrationOk", self.id);
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::RegistrationOk(self.id, src_id));
            }
            ServerMessages::RegistrationError(error) => {
                info!(
                    "Client {}: Received RegistrationError, cause: {}",
                    self.id,
                    error
                );
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::RegistrationError(self.id, src_id));
            }
            other => {
                debug!("Client {}: Received unhandled server message: {:?}", self.id, other);
            }
        }
    }

    /// Switches the client type (ChatClients <-> WebBrowsers).
    pub fn switch_client_type(&mut self) {
        info!("Client {}: Switching client type", self.id);
        self.client_type = match self.client_type {
            ClientType::ChatClients => ClientType::WebBrowsers,
            ClientType::WebBrowsers => ClientType::ChatClients,
        };
        info!("Client {}: New client type: {:?}", self.id, self.client_type);
    }

    /// Sends a Flood request to discover servers.
    pub fn server_discovery(&mut self) {
        info!("Client {}: Starting server discovery", self.id);

        // CLEAR CLIENT TOPOLOGY
        self.topology.clear();
        self.node_types.clear();

        let flood_request = FloodRequest {
            flood_id: generate_unique_id(),
            initiator_id: self.id,
            path_trace: vec![(self.id, NodeType::Client)],
        };

        // Sends a Flood request to all neighbors.
        for (&node_id, _) in &self.packet_send {
            info!("Client {}: Sending FloodRequest to node {}", self.id, node_id);
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

    /// Updates the network topology and node types based on the received path_trace.
    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        info!("Client {}: Updating graph with: {:?}", self.id, path_trace);
        // Adds edges to the graph (bidirectional).
        for i in 0..path_trace.len() - 1 {
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b));
            self.topology.insert((node_b, node_a));
        }
        debug!("Client {}: Updated topology: {:?}", self.id, self.topology);

        // Updates node types.
        for (node_id, node_type) in path_trace {
            self.node_types.insert(node_id, node_type);
        }
        debug!("Client {}: Updated node types: {:?}", self.id, self.node_types);
    }

    /// Calculates a route from the client to the target server using BFS.
    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        info!("Client {}: Calculating route to {}", self.id, target_server);
        info!("Client {}: Current topology: {:?}", self.id, self.topology);

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        // Map to track predecessors in the path.
        let mut predecessors: HashMap<NodeId, NodeId> = HashMap::new();

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current_node) = queue.pop_front() {
            debug!("Client {}: Current node in BFS: {}", self.id, current_node);

            // If the current node is the destination server, reconstructs the path and returns it.
            if current_node == *target_server {
                debug!("Client {}: Destination server {} found!", self.id, target_server);
                let mut path = Vec::new();
                let mut current = *target_server;
                // Reconstructs the path backward from predecessors.
                while let Some(&prev) = predecessors.get(&current) {
                    path.push(current);
                    current = prev;
                }
                path.push(self.id); // Adds the starting node (the client itself).
                path.reverse(); // Reverses the path to get the correct order.
                info!("Client {}: Path found: {:?}", self.id, path);
                return Some(path);
            }

            // Iterates over neighbors based on the *bidirectional* topology.
            for &(node_a, node_b) in &self.topology {
                // Checks neighbors in both directions.
                if node_a == current_node && !visited.contains(&node_b) {
                    debug!("Client {}: Exploring neighbor: {} of {}", self.id, node_b, node_a);
                    visited.insert(node_b);
                    queue.push_back(node_b);
                    predecessors.insert(node_b, node_a); // Stores the predecessor.
                } else if node_b == current_node && !visited.contains(&node_a) {
                    debug!("Client {}: Exploring neighbor: {} of {}", self.id, node_a, node_b);
                    visited.insert(node_a);
                    queue.push_back(node_a);
                    predecessors.insert(node_a, node_b); // Stores the predecessor.
                }
            }
        }

        // If no path is found, returns None.
        warn!("Client {}: No path found to {}", self.id, target_server);
        None
    }

    /// Sends a `ClientMessages` message to a server.
    fn send_client_message_to_server(
        &mut self,
        server_id: &NodeId,
        client_message: ClientMessages,
    ) {
        let message = TestMessage::WebServerMessages(client_message);
        // SAVE THE MESSAGE IN MESSAGE STORAGE
        self.send_message_to_node(server_id, message);
    }

    /// Sends a chat registration request to a server.
    pub fn register_with_server(&mut self, server_id: &NodeId) {
        info!("Client {}: Sending chat registration request to server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::RegistrationToChat);
    }

    /// Requests the list of connected clients from a server.
    pub fn request_client_list(&mut self, server_id: &NodeId) {
        info!("Client {}: Requesting client list from server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::ClientList);
    }

    /// Sends a message to another client via a server.
    pub fn send_message(&mut self, server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        info!("Client {}: Sending message \"{}\" to client {} via server {}", self.id, message_to_client, target_id, server_id);
        self.send_client_message_to_server(
            server_id,
            ClientMessages::MessageFor(target_id, message_to_client),
        );
    }

    /// Requests the list of available files from a server.
    pub fn request_file_list(&mut self, server_id: &NodeId) {
        info!("Client {}: Requesting file list from server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::FilesList);
    }

    /// Requests a specific file from a server.
    pub fn request_file(&mut self, server_id: &NodeId, file_id: u64) {
        info!("Client {}: Requesting file {} from server {}", self.id, file_id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::File(file_id));
    }

    /// Requests a specific media from a server.
    pub fn request_media(&mut self, server_id: &NodeId, file_id: u64) {
        info!("Client {}: Requesting media {} from server {}", self.id, file_id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::Media(file_id));
    }

    /// Requests the type of server.
    pub fn request_server_type(&mut self, server_id: &NodeId) {
        info!("Client {}: Requesting server type from server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::ServerType);
    }


    /// Sends a message (`TestMessage`) to a specific node.
    /// The message is fragmented and sent as a series of `MsgFragment` packets.
    fn send_message_to_node(&mut self, target_id: &NodeId, message: TestMessage) {
        // Clear excluded nodes at the start of sending a new message
        self.excluded_nodes.clear();

        // Calculate the path to the destination node.
        if let Some(path) = self.compute_route(target_id) {

            let session_id = generate_unique_id();
            let fragments = fragment_message(&message, path.clone(), session_id);
            self.pending_messages.insert(session_id, fragments.clone());

            // Sends fragments to the first hop of the path.
            if let (Some(next_hop), true) = (path.get(1), path.len() > 1) {
                if let Some(_) = self.packet_send.get(next_hop) {
                    for packet in fragments {
                        info!("Client {}: Sending packet to next hop {}", self.id, *next_hop);
                        self.send_packet_and_notify(packet, *next_hop);
                    }
                } else {
                    error!("Client {}: No sender for next hop {}", self.id, next_hop);
                }
            } else {
                error!("Client {}: Invalid path to {}", self.id, target_id);
            }
        } else {
            warn!("Client {}: No path to {}", self.id, target_id);
        }
    }

    /// Sends a packet to a recipient and notifies the simulation controller.
    fn send_packet_and_notify(&self, packet: Packet, recipient_id: NodeId) {
        if let Some(sender) = self.packet_send.get(&recipient_id) {
            if let Err(e) = sender.send(packet.clone()) {
                error!(
                    "Client {}: Error sending packet to {}: {:?}",
                    self.id,
                    recipient_id,
                    e
                );
            } else {
                info!(
                    "Client {}: Packet sent to {}: must arrive at {}",
                    self.id,
                    recipient_id,
                    packet.routing_header.hops.last().unwrap(),
                );

                // Notifies the simulation controller of packet sending.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::PacketSent(packet));
            }
        } else {
            error!("Client {}: No sender for node {}", self.id, recipient_id);
        }
    }

    /// Adds a neighbor to the sender map.
    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        info!("Client {}: Adding neighbor {}", self.id, node_id);
        if self.packet_send.insert(node_id, sender).is_some() {
            warn!("Client {}: Replaced existing sender for node {}", self.id, node_id);
        }
    }

    /// Removes a neighbor from the sender map.
    fn remove_neighbor(&mut self, node_id: &NodeId) {
        info!("Client {}: Removing neighbor {}", self.id, node_id);
        if self.packet_send.remove(node_id).is_none() {
            warn!("Client {}: Node {} was not a neighbor.", self.id, node_id);
        }
    }

    /// Handles a received `FloodRequest`.
    fn handle_flood_request(&mut self, packet: Packet) {
        // Extracts the FloodRequest from the packet.
        let flood_request = match packet.pack_type {
            PacketType::FloodRequest(req) => req,
            _ => {
                // Should never happen.
                error!("Client {}: handle_flood_request called with a non-FloodRequest packet type", self.id);
                return;
            }
        };

        info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request);

        // Gets the sender ID.
        let source_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                warn!("Client {}: FloodRequest without sender", self.id);
                return;
            }
        };

        // Updates the graph with path_trace information.
        self.update_graph(flood_request.path_trace.clone());

        // Prepares the path_trace for the response and inserts the client node.
        let mut response_path_trace = flood_request.path_trace.clone();
        response_path_trace.push((self.id, NodeType::Client));

        // Creates the FloodResponse.
        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: response_path_trace,
        };

        // Creates the response packet.
        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response.clone()),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                // Reverses the path_trace to return to the sender.
                hops: flood_request.path_trace.iter().rev().map(|(id, _)| *id).collect(),
            },
            session_id: packet.session_id,
        };

        info!("Client {}: Sending FloodResponse to {}, response packet: {:?}", self.id, source_id, response_packet);

        // Sends the FloodResponse to the sender.
        let next_node = response_packet.routing_header.hops[0];
        info!("Client {}: Sending FloodResponse via {}", self.id, next_node);
        self.send_packet_and_notify(response_packet, next_node);
    }

    fn handle_error(&mut self, error_msg: String) {
        let _ = self.sim_controller_send.send(
            ClientEvent::Error(self.id, error_msg)
        );
    }
}