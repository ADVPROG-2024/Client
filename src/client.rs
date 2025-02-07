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
    pub id: NodeId, // Unique identifier for the client.
    sim_controller_send: Sender<ClientEvent>, // Channel to send events to the simulation controller.
    sim_controller_recv: Receiver<ClientCommand>, // Channel to receive commands from the simulation controller.
    packet_recv: Receiver<Packet>, // Channel to receive packets from other nodes.
    packet_send: HashMap<NodeId, Sender<Packet>>, // Map of node IDs to channels for sending packets to those nodes.
    pub client_type: ClientType, // Type of client (e.g., ChatClients, WebBrowsers).
    message_storage: HashMap<(usize, NodeId), (Vec<u8>, Vec<bool>)>, // Stores fragments of messages for reassembly: (Session ID, Source Node ID) -> (Message Data, Received Fragments Flags).
    topology: HashSet<(NodeId, NodeId)>, // Set of edges representing the network topology (bidirectional).
    node_types: HashMap<NodeId, NodeType>, // Map of node IDs to their respective types (Client, Drone, Server).
}

impl DronegowskiClient {
    // Constructor for the DronegowskiClient.
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

        client.server_discovery(); // Initiate server discovery upon creation.
        client
    }

    // Main loop for the client.  It uses non-blocking channel selection.
    pub fn run(&mut self) {
        loop {
            select_biased! { // Prioritizes packets, then commands.
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

    // Handles commands received from the simulation controller.
    fn handle_client_command(&mut self, command: ClientCommand) {
        log::info!("Client {}: Received {:?}", self.id, command);
        match command {
            ClientCommand::RemoveSender(node_id) => {
                self.remove_neighbor(&node_id);
                self.server_discovery(); // Re-discover network after removing a neighbor
            }
            ClientCommand::AddSender(node_id, packet_sender) => {
                self.add_neighbor(node_id, packet_sender);
                self.server_discovery();  // Re-discover network after adding a neighbor
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

    // Handles incoming packets.
    fn handle_packet(&mut self, packet: Packet) {
        log::info!("Client {}: Received {:?}", self.id, packet);

        match packet.pack_type {
            PacketType::MsgFragment(_) => self.handle_message_fragment(packet), // Handle message fragments.
            PacketType::FloodResponse(flood_response) => { // Handle flood responses.
                log::info!(
                    "Client {}: Received FloodResponse: {:?}",
                    self.id,
                    flood_response
                );
                self.update_graph(flood_response.path_trace); // Update the network topology.
            }
            PacketType::FloodRequest(_) => self.handle_flood_request(packet), // Handle flood requests.
            Ack(session_id) => { // Handle acknowledgments (ACKs).
                log::info!("Client {}: Received Ack for session: {}", self.id, session_id);
            }
            PacketType::Nack(_) => {} // Handle negative acknowledgments (NACKs) - currently a no-op.
        }
    }

    // Handles the reassembly of fragmented messages.
    fn handle_message_fragment(&mut self, packet: Packet) {
        let fragment = match packet.pack_type {
            PacketType::MsgFragment(f) => f,
            _ => return, // Ignore non-fragment packets.
        };

        let src_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                log::warn!("Client {}: MsgFragment without source", self.id);
                return; // Ignore fragments without a source.
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

        // Block to limit the scope of the mutable borrow of self.message_storage
        let reassembled_data = {
            let key = (packet.session_id as usize, src_id); // Key for the message storage HashMap.
            // Get or insert the message data and fragment received flags.
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
                    // Initialize storage with enough capacity for the full message.
                    (
                        Vec::with_capacity((fragment.total_n_fragments * 128) as usize),
                        vec![false; fragment.total_n_fragments as usize], // Initialize all fragment flags to false.
                    )
                });

            assembler(message_data, &fragment); // Add the fragment's data to the message buffer.
            //Mark if is in bound
            if (fragment.fragment_index as usize) < fragments_received.len(){
                fragments_received[fragment.fragment_index as usize] = true;  // Mark the fragment as received.
            }


            let all_fragments_received = fragments_received.iter().all(|&received| received); // Check if all fragments have been received.

            let percentage = (fragments_received.iter().filter(|&&r| r).count() * 100) // Calculate the percentage of fragments received.
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
            // If all fragments are received return data
            if all_fragments_received {
                Some((packet.session_id, message_data.clone())) // Return the session ID and a *clone* of the message data.
            } else {
                None // Return None if not all fragments are received.
            }
        }; // End of the mutable borrow scope

        // If all fragments were received (reassembled_data is Some), process the message.
        if let Some((session_id, message_data)) = reassembled_data {
            self.process_reassembled_message(session_id, src_id, &message_data); // Process the reassembled message.
            self.message_storage.remove(&(session_id as usize, src_id)); // Remove the message from storage.
        }
    }
    // Processes a fully reassembled message.
    fn process_reassembled_message(&mut self, session_id: u64, src_id: NodeId, message_data: &[u8]) {
        match deserialize_message::<TestMessage>(message_data) { // Attempt to deserialize the message.
            Ok(TestMessage::WebServerMessages(client_message)) => { // If it's a WebServerMessages
                self.handle_server_message(src_id, client_message);   // Handle ServerMessages
            }
            Ok(deserialized_message) => { // If it's a generic TestMessage.
                log::info!("Client {}: Message from session {} from {} fully reassembled: {:?}", self.id, session_id, src_id, deserialized_message);
                let _ = self.sim_controller_send.send(ClientEvent::MessageReceived(deserialized_message)); // Send a MessageReceived event to the simulation controller.

            }
            Err(e) => { // If deserialization failed.
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

    // Handles messages specifically from the web server.
    fn handle_server_message(&mut self, src_id: NodeId, client_message: ClientMessages) {
        match client_message {
            ClientMessages::ServerMessages(server_message) => match server_message {
                ServerMessages::ServerType(server_type) => {
                    log::info!("Client {}: Received ServerType: {:?}", self.id, server_type);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ServerTypeReceived(src_id, server_type)); // Send ServerTypeReceived event.
                }
                ServerMessages::ClientList(clients) => {
                    log::info!("Client {}: Received ClientList: {:?}", self.id, clients);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ClientListReceived(src_id, clients)); // Send ClientListReceived event.
                }
                ServerMessages::FilesList(files) => {
                    log::info!("Client {}: Received FilesList: {:?}", self.id, files);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FilesListReceived(src_id, files)); // Send FilesListReceived event.
                }
                ServerMessages::File(file_data) => {
                    log::info!(
                        "Client {}: Received File data (size: {} bytes)",
                        self.id,
                        file_data.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FileReceived(src_id, file_data)); // Send FileReceived event.
                }
                ServerMessages::Media(media_data) => {
                    log::info!(
                        "Client {}: Received Media data (size: {} bytes)",
                        self.id,
                        media_data.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::MediaReceived(src_id, media_data)); // Send MediaReceived event.
                }
                ServerMessages::MessageFrom(from_id, message) => {
                    log::info!(
                        "Client {}: Received MessageFrom: {} from {}",
                        self.id,
                        message,
                        from_id
                    );
                    let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived( // Send MessageFromReceived event.
                                                                                            src_id, from_id, message,
                    ));
                }
                ServerMessages::RegistrationOk => {
                    log::info!("Client {}: Received RegistrationOk", self.id);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationOk(src_id)); // Send RegistrationOk event.
                }
                ServerMessages::RegistrationError(error) => {
                    log::info!(
                        "Client {}: Received RegistrationError, cause: {}",
                        self.id,
                        error
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationError(src_id)); // Send RegistrationError event.
                }
                _ => {} // Ignore unhandled server message variants.
            },
            _ => { // Ignore unhandled client message variants.

            }
        }
    }

    // Switches the client type between ChatClients and WebBrowsers.
    pub fn switch_client_type(&mut self) {
        log::info!("Client Type Switched");
        self.client_type = match self.client_type {
            ClientType::ChatClients => ClientType::WebBrowsers,
            ClientType::WebBrowsers => ClientType::ChatClients,
        };
    }

    // Initiates the server discovery process by sending FloodRequests.
    pub fn server_discovery(&self) {
        let flood_request = FloodRequest {
            flood_id: generate_unique_id(), // Generate a unique ID for the flood request.
            initiator_id: self.id, // Set the initiator ID to the client's ID.
            path_trace: vec![(self.id, NodeType::Client)], // Initialize the path trace with the client's ID and type.
        };

        // Iterate over all known neighbors and send a FloodRequest to each.
        for (&node_id, sender) in &self.packet_send {
            log::info!("Sending FloodRequest to node {}", node_id);
            let packet = Packet {
                pack_type: PacketType::FloodRequest(flood_request.clone()), // Clone the flood request for each neighbor.
                routing_header: SourceRoutingHeader {
                    hop_index: 0, // Set the initial hop index to 0.
                    hops: vec![self.id, node_id], // Set the initial hops to the client and the neighbor.
                },
                session_id: flood_request.flood_id, // Use the flood request's ID as the session ID.
            };
            self.send_packet_and_notify(packet, node_id); // Send the packet and notify the simulation controller.
        }
    }

    // Updates the network topology and node types based on a received path trace.
    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        log::info!("Updating graph with: {:?}", path_trace);
        // Iterate over pairs of nodes in the path trace to create edges.
        for i in 0..path_trace.len() - 1 {
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b)); // Insert the edge (a, b).
            self.topology.insert((node_b, node_a)); // Insert the edge (b, a) to represent bidirectionality.
        }

        // Update the node types.
        for (node_id, node_type) in path_trace {
            self.node_types.insert(node_id, node_type); // Insert/update the node type in the map.
        }
    }

    // Computes a route to a target server using Breadth-First Search (BFS).
    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        let mut visited = HashSet::new(); // Keep track of visited nodes.
        let mut queue = VecDeque::new(); // Queue for BFS.
        let mut predecessors = HashMap::new(); // Map to store the path (predecessor of each node).

        queue.push_back(self.id); // Start BFS from the client's ID.
        visited.insert(self.id); // Mark the client as visited.

        while let Some(current) = queue.pop_front() { // While the queue is not empty.
            if current == *target_server { // If the current node is the target server.
                let mut path = vec![current]; // Build the path by tracing back through predecessors.
                while let Some(&pred) = predecessors.get(&path[0]) {
                    path.insert(0, pred); // Insert the predecessor at the beginning of the path.
                }
                return Some(path); // Return the complete path.
            }

            // Iterate over neighbors based on the stored topology.
            for &(node_a, node_b) in &self.topology {
                if node_a == current && !visited.contains(&node_b) { // If node_a is the current node and node_b is not visited.
                    visited.insert(node_b); // Mark node_b as visited.
                    queue.push_back(node_b); // Add node_b to the queue.
                    predecessors.insert(node_b, current); // Set the predecessor of node_b to current.
                }
            }
        }

        None // Return None if no path to the target server is found.
    }

    // Helper function to send a client message to a server.
    fn send_client_message_to_server(
        &self,
        server_id: &NodeId,
        client_message: ClientMessages,
    ) {
        let message = TestMessage::WebServerMessages(client_message); // Wrap the ClientMessages in a TestMessage.
        self.send_message_to_node(server_id, message); // Send the message to the specified server.
    }

    // Public functions to handle various client requests (register, request client list, send message, etc.).
    pub fn register_with_server(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::RegistrationToChat); // Send a RegistrationToChat message.
    }

    pub fn request_client_list(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::ClientList); // Send a ClientList request.
    }

    pub fn send_message(&self, server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        self.send_client_message_to_server( // Send a MessageFor request.
                                            server_id,
                                            ClientMessages::MessageFor(target_id, message_to_client),
        );
    }

    pub fn request_file_list(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::FilesList); // Send a FilesList request.
    }

    pub fn request_file(&self, server_id: &NodeId, file_id: u64) {
        self.send_client_message_to_server(server_id, ClientMessages::File(file_id)); // Send a File request.
    }

    pub fn request_media(&self, server_id: &NodeId, file_id: u64) {
        self.send_client_message_to_server(server_id, ClientMessages::Media(file_id)); // Send a Media request.
    }

    pub fn request_server_type(&self, server_id: &NodeId) {
        self.send_client_message_to_server(server_id, ClientMessages::ServerType); // Send a ServerType request.
    }

    // Sends a TestMessage to a specified node.
    fn send_message_to_node(&self, target_id: &NodeId, message: TestMessage) {
        if let Some(path) = self.compute_route(target_id) { // Compute a route to the target node.
            let serialized_message = bincode::serialize(&message).expect("Serialization failed"); // Serialize the message.
            let fragments = fragment_message(&serialized_message, path.clone(), generate_unique_id()); // Fragment the message for transmission.

            // Get the next hop in the path
            if let (Some(next_hop), true) = (path.get(1), path.len() > 1) {
                if let Some(sender) = self.packet_send.get(next_hop) { // Get the sender channel for the next hop.
                    for packet in fragments { // Iterate over the message fragments.
                        self.send_packet_and_notify(packet, *next_hop); // Send each fragment and notify.
                    }
                } else {
                    log::error!("Client {}: No sender for next hop {}", self.id, next_hop);
                }
            } else {
                log::error!("Client {}: Invalid route to {}", self.id, target_id);
            }
        } else {
            log::warn!("Client {}: No route to {}", self.id, target_id); // Log a warning if no route is found.
        }
    }

    // Sends a packet and notifies the simulation controller.
    fn send_packet_and_notify(&self, packet: Packet, recipient_id: NodeId) {
        if let Some(sender) = self.packet_send.get(&recipient_id) { // Get the sender channel for the recipient.
            if let Err(e) = sender.send(packet.clone()) { // Attempt to send the packet.
                log::error!(
                    "Client {}: Failed to send packet to {}: {:?}",
                    self.id,
                    recipient_id,
                    e
                );
            } else {
                let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet)); // Notify the simulation controller.
            }
        } else {
            log::error!("Client {}: No sender for node {}", self.id, recipient_id);
        }
    }

    // Adds a neighbor to the client's list of neighbors.
    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        if self.packet_send.insert(node_id, sender).is_some() { // Insert the neighbor's ID and sender channel.
            log::warn!("Client {}: Replaced existing sender for node {}", self.id, node_id); // Log a warning if an existing entry was replaced.
        }
    }

    // Removes a neighbor from the client's list of neighbors.
    fn remove_neighbor(&mut self, node_id: &NodeId) {
        if self.packet_send.remove(node_id).is_none() { // Remove the neighbor's ID and sender channel.
            log::warn!("Client {}: Node {} was not a neighbor.", self.id, node_id); // Log a warning if the node wasn't a neighbor.
        }
    }

    // Handles FloodRequest packets.
    fn handle_flood_request(&mut self, packet: Packet) {
        let mut flood_request = match packet.pack_type {
            PacketType::FloodRequest(req) => req,
            _ => return, // Ignore non-FloodRequest packets.
        };

        log::info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request);

        flood_request.path_trace.push((self.id, NodeType::Client)); // Add the client to the path trace.

        let source_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                log::warn!("Client {}: FloodRequest without source", self.id);
                return; // Ignore requests without a source.
            }
        };

        // Create the FloodResponse.
        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id, // Use the same flood ID.
            path_trace: flood_request.path_trace.clone(), // Include the updated path trace.
        };

        // Create the response packet with the reversed path.
        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response),
            routing_header: SourceRoutingHeader {
                hop_index: 0,               // Reset hop index.
                hops: flood_request
                    .path_trace
                    .iter()
                    .rev() // Reverse the path trace.
                    .map(|(id, _)| *id)
                    .collect(),
            },
            session_id: packet.session_id, // Use the same session ID.
        };

        // Attempt to send the response with a timeout.
        if self
            .send_message_with_timeout(response_packet, source_id, Duration::from_millis(500))
            .is_ok()
        {
            log::info!("Client {}: Sent FloodResponse to {}", self.id, source_id);
            self.update_graph(flood_request.path_trace.clone()); // Update the graph after a *successful* send.
        } else {
            log::warn!(
                "Client {}: Timeout/Error sending FloodResponse to {}",
                self.id,
                source_id
            );
            //self.server_discovery(); // Trigger network rediscovery on failure.
        }
    }

    // Sends a message with a timeout.
    fn send_message_with_timeout(
        &self,
        packet: Packet,
        recipient_id: NodeId,
        timeout: Duration,
    ) -> Result<(), ()> {
        match self.packet_send.get(&recipient_id) { // Get the sender channel for the recipient.
            Some(sender) => match sender.send_timeout(packet.clone(), timeout) { // Attempt to send with a timeout.
                Ok(()) => { // If the send was successful.
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::PacketSent(packet)); // Notify the simulation controller.
                    Ok(())
                }
                Err(_) => { // If a timeout occurred.
                    log::warn!(
                        "Client {}: Timeout sending packet to {}",
                        self.id,
                        recipient_id
                    );
                    Err(()) // Return an error.
                }
            },
            None => { // If no sender channel is found for the recipient.
                log::warn!("Client {}: No sender for node {}", self.id, recipient_id);
                Err(()) // Return an error.
            }
        }
    }
}