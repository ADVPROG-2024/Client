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
/// and the client-specific logic (chat or web browsing)
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
    /// Map to store pending messages, where the key is the session ID and the value is a vector of packets (message fragments).
    pending_messages: HashMap<u64, Vec<Packet>>,
    /// Map to track acknowledged fragments for each session, key is session ID, value is a set of acknowledged fragment indices.
    acked_fragments: HashMap<u64, HashSet<u64>>, // Nuovo campo per tracciare i frammenti confermati
    /// Map to count NACKs for each fragment, session, and dropping node. Key is (fragment_index, session_id, dropping_node), value is the NACK counter.
    nack_counter: HashMap<(u64, u64, NodeId), u8>,
    /// Set of nodes that are currently excluded from routing paths due to repeated NACKs.
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

        // Performs initial server discovery when the client is created.
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
                        error!("Client {}: Error receiving packet", self.id); // Logged when there's an error receiving a packet from the `packet_recv` channel. Indicates a problem with the channel itself or the sender.
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    if let Ok(command) = command_res {
                        self.handle_client_command(command);
                    } else {
                        error!("Client {}: Error receiving command from simulator", self.id); // Logged when there's an error receiving a command from the `sim_controller_recv` channel. Indicates a problem with the channel or the simulator's ability to send commands.
                    }
                }
            }
        }
    }

    /// Handles a command received from the simulation controller.
    ///
    /// # Arguments
    ///
    /// * `command`: The command to handle.
    fn handle_client_command(&mut self, command: ClientCommand) {
        // info!("Client {}: Received command from simulator: {:?}", self.id, command); // Logged when the client receives a command from the simulation controller via `sim_controller_recv`. Useful for tracking which commands are being processed by the client.
        match command {
            ClientCommand::RemoveSender(node_id) => {
                // Removes a neighbor and re-executes server discovery to update network knowledge.
                self.remove_neighbor(&node_id);
                self.server_discovery();
            }
            ClientCommand::AddSender(node_id, packet_sender) => {
                // Adds a neighbor and re-executes server discovery to update network knowledge.
                self.add_neighbor(node_id, packet_sender);
                self.server_discovery();
            }
            ClientCommand::ServerType(node_id) => self.request_server_type(&node_id), // Requests server type from a specific node.
            ClientCommand::FilesList(node_id) => self.request_file_list(&node_id),   // Requests file list from a specific node.
            ClientCommand::File(node_id, file_id) => self.request_file(&node_id, file_id), // Requests a specific file from a node.
            ClientCommand::Media(node_id, media_id) => self.request_media(&node_id, media_id), // Requests specific media from a node.
            ClientCommand::ClientList(node_id) => self.request_client_list(&node_id), // Requests client list from a specific node.
            ClientCommand::RegistrationToChat(node_id) => self.register_with_server(&node_id), // Registers with a chat server.
            ClientCommand::MessageFor(node_id, client_id, message) => self.send_message(&node_id, client_id, message), // Sends a message to another client via a server.
            ClientCommand::RequestNetworkDiscovery => self.server_discovery(), // Initiates network discovery.
            ClientCommand::ControllerShortcut(packet) => self.handle_packet(packet), // Handles a packet directly sent from the controller (for testing or specific scenarios).
        }
    }

    /// Handles a received packet.
    ///
    /// # Arguments
    ///
    /// * `packet`: The packet to handle.
    fn handle_packet(&mut self, packet: Packet) {
        // info!("Client {}: Received packet: {:?}", self.id, packet); // Logged whenever the client receives any packet via `packet_recv`.  Logs the packet type and relevant information for debugging and monitoring network traffic.

        match packet.pack_type {
            PacketType::MsgFragment(_) => self.handle_message_fragment(packet), // Handles message fragments for reassembly.
            PacketType::FloodResponse(flood_response) => {
                // info!(
                //     "Client {}: Received FloodResponse: {:?}",
                //     self.id,
                //     flood_response
                // ); // Logged when the client receives a FloodResponse packet.  Indicates that a server discovery process is underway and the client is receiving network topology information.
                self.update_graph(flood_response.path_trace); // Updates network topology based on FloodResponse.
            }
            PacketType::FloodRequest(_) => self.handle_flood_request(packet), // Handles flood requests to participate in network discovery.
            PacketType::Ack(ack) => {
                // info!("Client {}: Received Ack for fragment {}", self.id, ack.fragment_index); // Logged when the client receives an Ack packet for a specific message fragment. Confirms successful delivery of a fragment to the recipient.
                let session_id = packet.session_id;
                let fragment_index = ack.fragment_index;

                // Rimuove le entry correlate dal nack_counter
                self.nack_counter.retain(|(f_idx, s_id, _), _| !(*f_idx == fragment_index && *s_id == session_id)); // Removes NACK counter entries for the acknowledged fragment.

                // Aggiorna acked_fragments
                let acked = self.acked_fragments.entry(session_id).or_default();
                acked.insert(fragment_index); // Marks the fragment as acknowledged.

                // Verifica se tutti i frammenti sono stati confermati
                if let Some(fragments) = self.pending_messages.get(&session_id) {
                    let total_fragments = fragments.len() as u64;
                    if acked.len() as u64 == total_fragments {
                        self.pending_messages.remove(&session_id); // Removes pending message session if all fragments are acknowledged.
                        self.acked_fragments.remove(&session_id); // Clears acknowledged fragments set for the session.
                        // info!("Client {}: All fragments for session {} have been acknowledged", self.id, session_id); // Logged when all fragments of a message session have been successfully acknowledged. Indicates successful message transmission.
                    }
                }
            }
            PacketType::Nack(ref nack) => {
                //info!("Client {}: Received Nack", self.id); // Logged when the client receives a Nack packet.  Indicates that a fragment was not successfully received by the next hop, triggering retransmission or alternative path calculation.
                let drop_drone = packet.clone().routing_header.hops[0];
                // NACK HANDLING METHOD
                self.handle_nack(nack.clone(), packet.session_id, drop_drone); // Handles Negative Acknowledgements (NACKs) for error recovery.
            }
        }
    }

    // Modifies the handle_nack function as follows
    /// Handles Negative Acknowledgements (NACKs) for packet fragments.
    ///
    /// # Arguments
    ///
    /// * `nack`: The Nack packet received.
    /// * `session_id`: The session ID of the message.
    /// * `id_drop_drone`: The ID of the node that dropped the packet (indicated by the NACK).
    fn handle_nack(&mut self, nack: Nack, session_id: u64, id_drop_drone: NodeId) {
        let key = (nack.fragment_index, session_id, id_drop_drone); // Key for NACK counter: (fragment index, session ID, dropping node).

        // Uses Entry to correctly handle counter initialization
        let counter = self.nack_counter.entry(key).or_insert(0); // Gets or initializes the NACK counter for this fragment, session and dropping node.
        *counter += 1; // Increments the NACK counter.

        let _ = self
            .sim_controller_send
            .send(ClientEvent::DebugMessage(self.id, format!("Client {}: nack {} from {}", self.id, counter, id_drop_drone)));

        match nack.nack_type {
            NackType::Dropped => {
                if *counter > 3 { // If NACK count exceeds 5 for a dropped fragment, consider alternative routing.

                    info!("Client {}: 10 NACKs from drone {} for fragment {}. Calculating alternative path", self.id, id_drop_drone, nack.fragment_index); // Logged when the number of NACKs (specifically of type 'Dropped') for a fragment exceeds a threshold (5 in this case). Triggers the process of finding an alternative path.

                    // Add the problematic node to excluded nodes
                    self.excluded_nodes.insert(id_drop_drone); // Adds the node that dropped the packet to the set of excluded nodes.

                    // Reconstruct the packet with a new path
                    if let Some(fragments) = self.pending_messages.get(&session_id) { // Retrieves the pending message fragments for the session.
                        if let Some(packet) = fragments.get(nack.fragment_index as usize) { // Gets the specific fragment that was NACKed.
                            if let Some(target_server) = packet.routing_header.hops.last() { // Gets the final destination server from the packet's routing header.
                                if let Some(new_path) = self.compute_route_excluding(target_server) { // Computes a new route to the target server, excluding problematic nodes.
                                    // sending route to SC
                                    let _ = self
                                        .sim_controller_send
                                        .send(ClientEvent::DebugMessage(self.id, format!("Client {}: new route exclude {:?}", self.id, self.excluded_nodes)));

                                    let _ = self
                                        .sim_controller_send
                                        .send(ClientEvent::Route(new_path.clone()));

                                    let mut new_packet = packet.clone();
                                    new_packet.routing_header.hops = new_path; // Updates the packet's routing header with the new path.
                                    new_packet.routing_header.hop_index = 1; // Resets hop index for the new path.

                                    if let Some(next_hop) = new_packet.routing_header.hops.get(1) { // Gets the next hop in the new path.
                                        info!("Client {}: Resending fragment {} via new path: {:?}",
                                            self.id, nack.fragment_index, new_packet.routing_header.hops); // Logged when a fragment is being resent using an alternative path due to excessive NACKs. Shows the new path being used.
                                        // add Client event

                                        self.send_packet_and_notify(new_packet.clone(), *next_hop); // Cloned here to fix borrow error, resends the fragment using the new path.

                                        // Reset the counter after rerouting
                                        // self.nack_counter.remove(&key); // Resets the NACK counter for this fragment after successful rerouting.
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    warn!("Client {}: Unable to find alternative path", self.id); // Logged as a warning if the client fails to find an alternative path after receiving too many NACKs. Indicates potential delivery issues.
                } else {
                    // Standard resend if NACK count is not too high
                    if let Some(fragments) = self.pending_messages.get(&session_id) { // Retrieves pending message fragments.
                        if let Some(packet) = fragments.get(nack.fragment_index as usize) { // Gets the NACKed fragment.
                            // info!("Client {}: Attempt {} for fragment {}",
                            // self.id, counter, nack.fragment_index); // Logged before resending a fragment after receiving a NACK, indicating the attempt number for retransmission.
                            self.send_packet_and_notify(packet.clone(), packet.routing_header.hops[1]); // Resends the fragment to the original next hop.
                        }
                    }
                }
            }
            _ => {
                // Handling other NACK types (e.g., Corrupted). For now, triggers server discovery and resends fragment.
                self.server_discovery(); // Re-initiates server discovery, potentially network topology has changed.
                if let Some(fragments) = self.pending_messages.get(&session_id) { // Retrieves pending message fragments.
                    if let Some(packet) = fragments.get(nack.fragment_index as usize) { // Gets the NACKed fragment.
                        self.send_packet_and_notify(packet.clone(), packet.routing_header.hops[1]); // Resends the fragment to the original next hop.
                    }
                }
            }
        }
    }

    // New function for calculating paths excluding nodes
    /// Computes a route to a target server, excluding nodes that have been marked as problematic (excluded_nodes).
    ///
    /// # Arguments
    ///
    /// * `target_server`: The NodeId of the target server.
    ///
    /// # Returns
    ///
    /// `Some(Vec<NodeId>)` if a route is found, `None` otherwise.
    fn compute_route_excluding(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        let mut visited = HashSet::new(); // Set to keep track of visited nodes during BFS.
        let mut queue = VecDeque::new(); // Queue for BFS traversal.
        let mut predecessors = HashMap::new(); // Map to store predecessors for path reconstruction.

        queue.push_back(self.id); // Start BFS from the client's own ID.
        visited.insert(self.id); // Mark client node as visited.

        while let Some(current_node) = queue.pop_front() { // While there are nodes in the queue.
            if current_node == *target_server { // If the current node is the target server.
                let mut path = Vec::new();
                let mut current = *target_server;
                while let Some(prev) = predecessors.get(&current) { // Reconstruct path by backtracking from the target server using predecessors.
                    path.push(current);
                    current = *prev;
                }
                path.push(self.id); // Add the client's ID to the path.
                path.reverse(); // Reverse the path to get the correct order from client to server.
                return Some(path); // Return the computed path.
            }

            // Iterate over neighbors excluding problematic nodes
            for &(a, b) in &self.topology { // Iterate through the network topology (edges).
                if a == current_node && !self.excluded_nodes.contains(&b) && !visited.contains(&b) { // If 'b' is a neighbor of 'a', 'b' is not excluded, and 'b' is not visited.
                    visited.insert(b); // Mark 'b' as visited.
                    queue.push_back(b); // Add 'b' to the queue for further exploration.
                    predecessors.insert(b, a); // Set 'a' as the predecessor of 'b'.
                } else if b == current_node && !self.excluded_nodes.contains(&a) && !visited.contains(&a) { // If 'a' is a neighbor of 'b', 'a' is not excluded and 'a' is not visited.
                    visited.insert(a); // Mark 'a' as visited.
                    queue.push_back(a); // Add 'a' to the queue.
                    predecessors.insert(a, b); // Set 'b' as the predecessor of 'a'.
                }
            }
        }

        let _ = self.sim_controller_send.send(ClientEvent::Error(self.id, "not alternative path route available".to_string()));
        None // Return None if no path is found.
    }


    /// Handles a received message fragment.
    ///
    /// # Arguments
    ///
    /// * `packet`: The packet containing the message fragment.
    fn handle_message_fragment(&mut self, packet: Packet) {
        let fragment = match packet.pack_type {
            PacketType::MsgFragment(f) => f, // Extracts the message fragment from the packet.
            _ => {
                // Should never happen, as this function is only called for MsgFragment.
                error!("Client {}: handle_message_fragment called with a non-MsgFragment packet type", self.id); // Logged as an error if `handle_message_fragment` is called with a packet that is not of type `MsgFragment`. This should not happen under normal program flow and indicates a programming error.
                return;
            }
        };

        let src_id = match packet.routing_header.source() {
            Some(id) => id, // Gets the source ID from the routing header.
            None => {
                warn!("Client {}: MsgFragment without sender", self.id); // Logged as a warning if a `MsgFragment` packet is received without a source ID in the routing header. Indicates a malformed packet.
                return;
            }
        };

        let _ = self
            .sim_controller_send
            .send(ClientEvent::DebugMessage(self.id, format!("Client {}: received from {}", self.id, src_id)));

        // info!(
        //     "Client {}: Received MsgFragment from: {}, Session: {}, Index: {}, Total: {}",
        //     self.id,
        //     src_id,
        //     packet.session_id,
        //     fragment.fragment_index,
        //     fragment.total_n_fragments
        // ); // Logged when a message fragment is received. Provides details about the sender, session ID, fragment index, and total number of fragments for the message.

        // Initialize ack_packet and next_hop outside the reassembled_data block
        let mut ack_packet_option: Option<Packet> = None; // Option to hold the Ack packet to be sent.
        let mut next_hop_option: Option<NodeId> = None; // Option to hold the next hop for sending the Ack packet.

        // Logic for reassembling fragments.
        let reassembled_data = {
            let key = (packet.session_id as usize, src_id); // Key for message storage: (session ID, sender ID).
            // Gets or inserts a new entry in the `message_storage` map.
            let (message_data, fragments_received) = self
                .message_storage
                .entry(key)
                .or_insert_with(|| {
                    // info!(
                    //     "Client {}: Initializing storage for session {} from {}",
                    //     self.id,
                    //     packet.session_id,
                    //     src_id
                    // ); // Logged when the client initializes storage for a new message session from a particular sender.  Indicates the start of reassembling a fragmented message.
                    // Initializes the vector for message data and the vector to track received fragments.
                    (
                        Vec::with_capacity((fragment.total_n_fragments * 128) as usize), // Pre-allocate vector for message data.
                        vec![false; fragment.total_n_fragments as usize], // Initialize boolean vector to track received fragments.
                    )
                });

            // Assembles the current fragment.
            assembler(message_data, &fragment); // Appends the fragment's data to the message data buffer.
            // Marks the fragment as received.
            // Marks the fragment as received.
            if (fragment.fragment_index as usize) < fragments_received.len() {
                fragments_received[fragment.fragment_index as usize] = true;
            } else {
                error!("Client {}: Fragment index {} out of bounds for session {} from {}", self.id, fragment.fragment_index, packet.session_id, src_id); // Logged as an error if a received fragment's index is out of bounds for the expected number of fragments. Indicates a problem with fragment numbering or message construction.
                return;
            }

            // Invia un Ack al mittente per confermare la ricezione del pacchetto/frammento
            // Send an Ack to the sender to confirm reception of the packet/fragment
            let reversed_hops: Vec<NodeId> = packet.routing_header.hops.iter().rev().cloned().collect(); // Reverses the path to send Ack back to sender.
            let ack_routing_header = SourceRoutingHeader {
                hop_index: 1,
                hops: reversed_hops, // Routing header for the Ack packet.
            };

            let ack_packet = Packet::new_ack(
                ack_routing_header,
                packet.session_id,
                fragment.fragment_index, // Creates a new Ack packet.
            );

            if let Some(next_hop) = ack_packet.routing_header.hops.get(1).cloned() { // Gets the next hop for sending the Ack.
                // info!("Client {}: Sending Ack for fragment {} to {}", self.id, fragment.fragment_index, next_hop); // Logged just before sending an Ack packet for a received fragment. Confirms that an Ack is being sent and to whom.
                // Store ack_packet and next_hop for sending after mutable borrow ends
                ack_packet_option = Some(ack_packet); // Stores the Ack packet for sending later.
                next_hop_option = Some(next_hop); // Stores the next hop for sending the Ack later.
            } else {
                warn!("Client {}: No valid path to send Ack for fragment {}", self.id, fragment.fragment_index); // Logged as a warning if there's no valid next hop to send an Ack packet to.  Indicates a routing issue when trying to acknowledge a fragment.
            }

            // Checks if all fragments have been received.
            let all_fragments_received = fragments_received.iter().all(|&received| received); // Checks if all fragments for this session have been marked as received.


            // let percentage = (fragments_received.iter().filter(|&&r| r).count() * 100) / fragment.total_n_fragments as usize;
            // info!(
            //     "Client {}: Fragment {}/{} for session {} from {}. {}% complete.",
            //     self.id,
            //     fragment.fragment_index + 1,
            //     fragment.total_n_fragments,
            //     packet.session_id,
            //     src_id,
            //     percentage
            // ); // Logged periodically as fragments are received, showing the progress of message reassembly. Displays current fragment number, total fragments, session ID, sender, and completion percentage.


            if all_fragments_received {
                // If all fragments have been received, returns the message data.
                Some((packet.session_id, message_data.clone())) // Return the session ID and reassembled message data if all fragments received.
            } else {
                // Otherwise, returns None.
                None // Return None if not all fragments are received yet.
            }
        };

        // Send Ack packet after the mutable borrow of message_storage has ended
        if let (Some(ack_packet), Some(next_hop)) = (ack_packet_option, next_hop_option) { // Send Ack packet if it was created and next hop is available.
            self.send_packet_and_notify(ack_packet, next_hop); // Sends the Ack packet.
        }


        // If the message has been reassembled, processes it.
        if let Some((session_id, message_data)) = reassembled_data { // If reassembled data is available.
            self.process_reassembled_message(session_id, src_id, &message_data); // Processes the reassembled message.
            // Removes the entry from the `message_storage` map.
            self.message_storage.remove(&(session_id as usize, src_id)); // Cleans up message storage after successful reassembly and processing.
            // info!("Client {}: Message from session {} from {} removed from storage", self.id, session_id, src_id); // Logged after a message session has been fully reassembled and processed. Indicates that the storage for this session is no longer needed and has been cleaned up.
        }
    }

    /// Processes a reassembled message.
    ///
    /// # Arguments
    ///
    /// * `session_id`: The session ID of the message.
    /// * `src_id`: The ID of the sender.
    /// * `message_data`: The reassembled message data as bytes.
    fn process_reassembled_message(&mut self, session_id: u64, src_id: NodeId, message_data: &[u8]) {
        // Deserializes the message.
        match bincode::deserialize(message_data) { // Attempts to deserialize the message data.
            Ok(TestMessage::WebClientMessages(server_message)) => {
                // If the message is a web server message, handles it.
                self.handle_server_message(src_id, server_message); // Handles server-specific messages.
            }
            Ok(deserialized_message) => {
                // info!(
                //     "Client {}: Message from session {} from {} completely reassembled: {:?}",
                //     self.id,
                //     session_id,
                //     src_id,
                //     deserialized_message
                // ); // Logged when a complete message has been reassembled and successfully deserialized. Shows the session ID, sender, and the deserialized message content.
                // Sends the received message to the simulation controller
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MessageReceived(deserialized_message)); // Sends the deserialized message to the simulation controller.
            }
            Err(e) => {
                error!(
                    "Client {}: Error deserializing session {} from {}: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    e
                ); // Logged as an error if there is a failure during deserialization of a reassembled message. Indicates data corruption or incompatibility between sender and receiver message formats.
            }
        }
    }

    /// Handles a message received from a server.
    ///
    /// # Arguments
    ///
    /// * `src_id`: The ID of the server sending the message.
    /// * `server_message`: The deserialized server message.
    fn handle_server_message(&mut self, src_id: NodeId, server_message: ServerMessages) {

        match server_message {
            ServerMessages::ServerType(server_type) => {
                // info!("Client {}: Received ServerType: {:?}", self.id, server_type); // Logged when a ServerType message is received from a server. Indicates the type of server (e.g., Web Server, Chat Server).
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::ServerTypeReceived(self.id, src_id, server_type)); // Sends ServerType information to the simulation controller.
            }
            ServerMessages::ClientList(clients) => {
                // info!("Client {}: Received ClientList: {:?}", self.id, clients); // Logged when a ClientList message is received from a server.  Contains a list of clients connected to that server.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::ClientListReceived(self.id, src_id, clients)); // Sends ClientList information to the simulation controller.
            }
            ServerMessages::FilesList(files) => {
                // info!("Client {}: Received FilesList: {:?}", self.id, files); // Logged when a FilesList message is received from a server. Contains a list of files available on the server.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::FilesListReceived(self.id, src_id, files)); // Sends FilesList information to the simulation controller.
            }
            ServerMessages::File(file_data) => {
                // info!(
                //     "Client {}: Received file data (size: {} bytes)",
                //     self.id,
                //     file_data.text.len()
                // ); // Logged when file data is received from a server. Shows the size of the received file data in bytes.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::FileReceived(self.id, src_id, file_data)); // Sends File data to the simulation controller.
            }
            ServerMessages::Media(media_data) => {
                // info!(
                //     "Client {}: Received media data (size: {} bytes)",
                //     self.id,
                //     media_data.len()
                // ); // Logged when media data is received from a server. Shows the size of the received media data in bytes.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MediaReceived(self.id, src_id, media_data)); // Sends Media data to the simulation controller.
            }
            ServerMessages::MessageFrom(from_id, message) => {
                // info!(
                //     "Client {}: Received MessageFrom: {} from {}",
                //     self.id,
                //     message,
                //     from_id
                // ); // Logged when a message intended for this client from another client (relayed through the server) is received. Shows the message content and the original sender's ID.
                let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived(
                    self.id, src_id, from_id, message, // Sends MessageFrom information to the simulation controller.
                ));
            }
            ServerMessages::RegistrationOk => {
                // info!("Client {}: Received RegistrationOk", self.id); // Logged when a registration request to a server is successful.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::RegistrationOk(self.id, src_id)); // Sends RegistrationOk event to the simulation controller.
            }
            ServerMessages::RegistrationError(_) => {
                // info!(
                //     "Client {}: Received RegistrationError, cause: {}",
                //     self.id,
                //     error
                // ); // Logged when a registration request to a server fails. Includes the error message describing the reason for failure.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::RegistrationError(self.id, src_id)); // Sends RegistrationError event to the simulation controller.
            }
            ServerMessages::Error(error) => {
                // info!(
                //     "Client {}: Received Error, cause: {}",
                //     self.id,
                //     error
                // ); // Logged when a generic error message is received from a server. Includes the error message.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::Error(self.id, error)); // Sends Error event to the simulation controller.
            }
        }
    }

    /// Switches the client type (ChatClients <-> WebBrowsers).
    pub fn switch_client_type(&mut self) {
        // info!("Client {}: Switching client type", self.id); // Logged when the client type is being switched (from ChatClients to WebBrowsers or vice versa).
        self.client_type = match self.client_type {
            ClientType::ChatClients => ClientType::WebBrowsers, // Switches from ChatClients to WebBrowsers.
            ClientType::WebBrowsers => ClientType::ChatClients, // Switches from WebBrowsers to ChatClients.
        };
        // info!("Client {}: New client type: {:?}", self.id, self.client_type); // Logged after the client type has been switched, showing the new client type.
    }

    /// Sends a Flood request to discover servers.
    pub fn server_discovery(&mut self) {
        // info!("Client {}: Starting server discovery", self.id); // Logged when the server discovery process is initiated by the client.

        // CLEAR CLIENT TOPOLOGY
        self.topology.clear(); // Clears the current network topology knowledge.
        self.node_types.clear(); // Clears the current node type knowledge.

        let flood_request = FloodRequest {
            flood_id: generate_unique_id(), // Generates a unique ID for the flood request.
            initiator_id: self.id, // Sets the initiator ID to the client's own ID.
            path_trace: vec![(self.id, NodeType::Client)], // Initializes path trace with the client's own node and type.
        };

        // Sends a Flood request to all neighbors.
        for (&node_id, _) in &self.packet_send { // Iterates through all known neighbors.
            // info!("Client {}: Sending FloodRequest to node {}", self.id, node_id); // Logged before sending a FloodRequest packet to each neighbor. Indicates the start of the flood process to discover network topology.
            let packet = Packet {
                pack_type: PacketType::FloodRequest(flood_request.clone()), // Sets packet type to FloodRequest.
                routing_header: SourceRoutingHeader {
                    hop_index: 0,
                    hops: vec![self.id, node_id], // Sets routing header with path from client to neighbor.
                },
                session_id: flood_request.flood_id, // Sets session ID to the flood request ID.
            };
            self.send_packet_and_notify(packet, node_id); // Sends the FloodRequest packet to the neighbor.
        }
    }

    /// Updates the network topology and node types based on the received path_trace.
    ///
    /// # Arguments
    ///
    /// * `path_trace`: A vector of (NodeId, NodeType) representing the discovered path.
    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        // info!("Client {}: Updating graph with: {:?}", self.id, path_trace); // Logged when the client is updating its network topology graph based on a received `path_trace`. Shows the path trace being used to update the graph.
        // Adds edges to the graph (bidirectional).
        for i in 0..path_trace.len() - 1 { // Iterates through the path trace to extract edges.
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b)); // Adds edge (a, b) to the topology.
            self.topology.insert((node_b, node_a)); // Adds edge (b, a) to make it bidirectional.
        }
        debug!("Client {}: Updated topology: {:?}", self.id, self.topology); // Debug log showing the updated topology after processing a path trace. Useful for detailed network analysis.

        // Updates node types.
        for (node_id, node_type) in path_trace { // Iterates through the path trace to update node types.
            self.node_types.insert(node_id, node_type); // Inserts or updates the node type in the node_types map.
        }
        debug!("Client {}: Updated node types: {:?}", self.id, self.node_types); // Debug log showing the updated node types after processing a path trace. Useful for detailed network analysis.
    }

    /// Calculates a route from the client to the target server using BFS.
    ///
    /// # Arguments
    ///
    /// * `target_server`: The NodeId of the target server.
    ///
    /// # Returns
    ///
    /// `Some(Vec<NodeId>)` if a route is found, `None` otherwise.
    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        // info!("Client {}: Calculating route to {}", self.id, target_server); // Logged when the client starts calculating a route to a specific target server.
        // info!("Client {}: Current topology: {:?}", self.id, self.topology); // Logged before route calculation, showing the current network topology known to the client. Useful for understanding the context of route calculation.

        let mut visited = HashSet::new(); // Set to keep track of visited nodes during BFS.
        let mut queue = VecDeque::new(); // Queue for BFS traversal.
        // Map to track predecessors in the path.
        let mut predecessors: HashMap<NodeId, NodeId> = HashMap::new(); // Map to store predecessor nodes during BFS for path reconstruction.

        queue.push_back(self.id); // Start BFS from the client's own ID.
        visited.insert(self.id); // Mark client node as visited.

        while let Some(current_node) = queue.pop_front() { // While there are nodes in the queue.
            debug!("Client {}: Current node in BFS: {}", self.id, current_node); // Debug log showing the currently explored node during the Breadth-First Search (BFS) route calculation. Useful for tracing the BFS algorithm.

            // If the current node is the destination server, reconstructs the path and returns it.
            if current_node == *target_server { // If the current node is the target server.
                debug!("Client {}: Destination server {} found!", self.id, target_server); // Debug log indicating that the destination server has been found during BFS.
                let mut path = Vec::new();
                let mut current = *target_server;
                // Reconstructs the path backward from predecessors.
                while let Some(&prev) = predecessors.get(&current) { // Backtrack from the target server to the client using predecessor information.
                    path.push(current);
                    current = prev;
                }
                path.push(self.id); // Adds the starting node (the client itself).
                path.reverse(); // Reverses the path to get the correct order.
                // info!("Client {}: Path found: {:?}", self.id, path); // Logged when a route to the target server is successfully found. Shows the calculated path.
                return Some(path); // Return the computed path.
            }

            // Iterates over neighbors based on the *bidirectional* topology.
            for &(node_a, node_b) in &self.topology { // Iterate through the network topology (edges).
                // Checks neighbors in both directions.
                if node_a == current_node && !visited.contains(&node_b) { // If 'b' is a neighbor of 'a' and 'b' has not been visited yet.
                    debug!("Client {}: Exploring neighbor: {} of {}", self.id, node_b, node_a); // Debug log indicating exploration of a neighbor node during BFS.
                    visited.insert(node_b); // Mark 'b' as visited.
                    queue.push_back(node_b); // Add 'b' to the queue for further exploration.
                    predecessors.insert(node_b, node_a); // Stores the predecessor.
                } else if node_b == current_node && !visited.contains(&node_a) { // If 'a' is a neighbor of 'b' and 'a' has not been visited yet.
                    debug!("Client {}: Exploring neighbor: {} of {}", self.id, node_a, node_b); // Debug log indicating exploration of a neighbor node during BFS.
                    visited.insert(node_a); // Mark 'a' as visited.
                    queue.push_back(node_a); // Add 'a' to the queue.
                    predecessors.insert(node_a, node_b); // Stores the predecessor.
                }
            }
        }

        // If no path is found, returns None.
        warn!("Client {}: No path found to {}", self.id, target_server); // Logged as a warning if no route to the target server could be found. Indicates network connectivity issues or that the server is unreachable.
        None // Return None if no path to the target server is found.
    }

    /// Sends a `ClientMessages` message to a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to send the message to.
    /// * `client_message`: The `ClientMessages` enum to send.
    fn send_client_message_to_server(
        &mut self,
        server_id: &NodeId,
        client_message: ClientMessages,
    ) {
        let message = TestMessage::WebServerMessages(client_message); // Wraps the ClientMessages in a TestMessage for sending.
        // SAVE THE MESSAGE IN MESSAGE STORAGE
        self.send_message_to_node(server_id, message); // Sends the message to the server node.
    }

    /// Sends a chat registration request to a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the chat server to register with.
    pub fn register_with_server(&mut self, server_id: &NodeId) {
        // info!("Client {}: Sending chat registration request to server {}", self.id, server_id); // Logged when the client is sending a registration request to a chat server.
        self.send_client_message_to_server(server_id, ClientMessages::RegistrationToChat); // Sends a registration request to the specified server.
    }

    /// Requests the list of connected clients from a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to request the client list from.
    pub fn request_client_list(&mut self, server_id: &NodeId) {
        // info!("Client {}: Requesting client list from server {}", self.id, server_id); // Logged when the client requests a list of clients connected to a server.
        self.send_client_message_to_server(server_id, ClientMessages::ClientList); // Sends a client list request to the specified server.
    }

    /// Sends a message to another client via a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to route the message through.
    /// * `target_id`: The ID of the client to send the message to.
    /// * `message_to_client`: The message string to send.
    pub fn send_message(&mut self, server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        // info!("Client {}: Sending message \"{}\" to client {} via server {}", self.id, message_to_client, target_id, server_id); // Logged when the client is sending a message to another client through a server. Shows the message content, target client ID, and the server being used.
        self.send_client_message_to_server(
            server_id,
            ClientMessages::MessageFor(target_id, message_to_client), // Sends a message for another client to the server.
        );
    }

    /// Requests the list of available files from a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to request the file list from.
    pub fn request_file_list(&mut self, server_id: &NodeId) {
        // info!("Client {}: Requesting file list from server {}", self.id, server_id); // Logged when the client is requesting a list of files from a server.
        self.send_client_message_to_server(server_id, ClientMessages::FilesList); // Sends a file list request to the specified server.
    }

    /// Requests a specific file from a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to request the file from.
    /// * `file_id`: The ID of the file to request.
    pub fn request_file(&mut self, server_id: &NodeId, file_id: u64) {
        // info!("Client {}: Requesting file {} from server {}", self.id, file_id, server_id); // Logged when the client is requesting a specific file from a server. Shows the file ID and the server.
        self.send_client_message_to_server(server_id, ClientMessages::File(file_id)); // Sends a file request to the specified server for a specific file ID.
    }

    /// Requests a specific media from a server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to request the media from.
    /// * `file_id`: The ID of the media to request.
    pub fn request_media(&mut self, server_id: &NodeId, file_id: u64) {
        // info!("Client {}: Requesting media {} from server {}", self.id, file_id, server_id); // Logged when the client is requesting specific media from a server. Shows the media ID and the server.
        self.send_client_message_to_server(server_id, ClientMessages::Media(file_id)); // Sends a media request to the specified server for a specific media ID.
    }

    /// Requests the type of server.
    ///
    /// # Arguments
    ///
    /// * `server_id`: The ID of the server to request the type from.
    pub fn request_server_type(&mut self, server_id: &NodeId) {
        info!("Client {}: Requesting server type from server {}", self.id, server_id); // Logged when the client is requesting the type of a server (e.g., web or chat server).
        self.send_client_message_to_server(server_id, ClientMessages::ServerType); // Sends a server type request to the specified server.
    }


    /// Sends a message (`TestMessage`) to a specific node.
    /// The message is fragmented and sent as a series of `MsgFragment` packets.
    ///
    /// # Arguments
    ///
    /// * `target_id`: The ID of the destination node.
    /// * `message`: The `TestMessage` to send.
    fn send_message_to_node(&mut self, target_id: &NodeId, message: TestMessage) {
        // Clear excluded nodes at the start of sending a new message
        self.excluded_nodes.clear(); // Clears the set of excluded nodes before sending a new message.

        // Calculate the path to the destination node.
        if let Some(path) = self.compute_route(target_id) { // Computes a route to the target node.

            // sending route to SC
            let _ = self
                .sim_controller_send
                .send(ClientEvent::Route(path.clone()));

            let session_id = generate_unique_id(); // Generates a unique session ID for the message transmission.
            let fragments = fragment_message(&message, path.clone(), session_id); // Fragments the message into packets.
            self.pending_messages.insert(session_id, fragments.clone()); // Stores the fragments as pending messages for this session.

            // Sends fragments to the first hop of the path.
            if let (Some(next_hop), true) = (path.get(1), path.len() > 1) { // Checks if there is a valid next hop in the calculated path.
                if let Some(_) = self.packet_send.get(next_hop) { // Checks if there is a sender channel for the next hop.
                    for packet in fragments { // Iterates through each fragment.
                        info!("Client {}: Sending packet to next hop {}", self.id, *next_hop); // Logged before sending each fragment of a message to the next hop in the calculated path.
                        self.send_packet_and_notify(packet, *next_hop); // Sends each fragment to the next hop.
                    }
                } else {
                    error!("Client {}: No sender for next hop {}", self.id, next_hop); // Logged as an error if there's no sender (channel) associated with the next hop in the calculated path. Indicates a configuration or neighbor issue.
                }
            } else {
                error!("Client {}: Invalid path to {}", self.id, target_id); // Logged as an error if the calculated path is invalid (e.g., empty or too short). Indicates a routing problem.
            }
        } else {
            warn!("Client {}: No path to {}", self.id, target_id); // Logged as a warning if no path could be computed to the target node. Indicates the target is unreachable.
        }
    }

    /// Sends a packet to a recipient and notifies the simulation controller.
    ///
    /// # Arguments
    ///
    /// * `packet`: The packet to send.
    /// * `recipient_id`: The ID of the recipient node.
    fn send_packet_and_notify(&self, packet: Packet, recipient_id: NodeId) {
        if let Some(sender) = self.packet_send.get(&recipient_id) { // Gets the sender channel for the recipient.
            if let Err(e) = sender.send(packet.clone()) { // Attempts to send the packet through the channel.
                error!(
                    "Client {}: Error sending packet to {}: {:?}",
                    self.id,
                    recipient_id,
                    e
                ); // Logged as an error if there's an issue sending a packet through the channel to the recipient. Indicates a problem with the channel or the recipient's receiver.
            } else {
                // info!(
                //     "Client {}: Packet sent to {}: must arrive at {}",
                //     self.id,
                //     recipient_id,
                //     packet.routing_header.hops.last().unwrap(),
                // ); // Logged after successfully sending a packet to a recipient. Shows the recipient and the final destination node of the packet.

                // Notifies the simulation controller of packet sending.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::PacketSent(packet)); // Notifies the simulation controller that a packet has been sent.
            }
        } else {
            error!("Client {}: No sender for node {}", self.id, recipient_id); // Logged as an error if there is no sender (channel) associated with the recipient ID. Indicates a missing neighbor or configuration issue.
        }
    }

    /// Adds a neighbor to the sender map.
    ///
    /// # Arguments
    ///
    /// * `node_id`: The ID of the neighbor node.
    /// * `sender`: The sender channel to the neighbor.
    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        // info!("Client {}: Adding neighbor {}", self.id, node_id); // Logged when a new neighbor is added to the client's neighbor list.
        if self.packet_send.insert(node_id, sender).is_some() { // Inserts the neighbor and sender channel into the packet_send map.
            warn!("Client {}: Replaced existing sender for node {}", self.id, node_id); // Logged as a warning if adding a neighbor replaces an existing entry for the same node ID. Indicates a potential configuration update or change in neighbors.
        }
    }

    /// Removes a neighbor from the sender map.
    ///
    /// # Arguments
    ///
    /// * `node_id`: The ID of the neighbor node to remove.
    fn remove_neighbor(&mut self, node_id: &NodeId) {
        // info!("Client {}: Removing neighbor {}", self.id, node_id); // Logged when a neighbor is removed from the client's neighbor list.
        if self.packet_send.remove(node_id).is_none() { // Removes the neighbor from the packet_send map.
            warn!("Client {}: Node {} was not a neighbor.", self.id, node_id); // Logged as a warning if an attempt is made to remove a neighbor that is not currently in the neighbor list. Indicates an inconsistency in neighbor management.
        }
    }

    /// Handles a received `FloodRequest`.
    ///
    /// # Arguments
    ///
    /// * `packet`: The packet containing the FloodRequest.
    fn handle_flood_request(&mut self, packet: Packet) {
        // Extracts the FloodRequest from the packet.
        let flood_request = match packet.pack_type {
            PacketType::FloodRequest(req) => req, // Extracts the FloodRequest from the packet.
            _ => {
                // Should never happen.
                error!("Client {}: handle_flood_request called with a non-FloodRequest packet type", self.id); // Logged as an error if `handle_flood_request` is called with a packet that is not of type `FloodRequest`. This is a programming error.
                return;
            }
        };

        info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request); // Logged when the client receives a FloodRequest packet, indicating the start of network discovery by another node.

        // // Gets the sender ID.
        // let source_id = match packet.routing_header.source() {
        //     Some(id) => id, // Gets the source ID from the routing header.
        //     None => {
        //         warn!("Client {}: FloodRequest without sender", self.id); // Logged as a warning if a FloodRequest packet is received without a source ID. Indicates a malformed packet.
        //         return;
        //     }
        // };

        // Updates the graph with path_trace information.
        self.update_graph(flood_request.path_trace.clone()); // Updates the network topology based on the received path trace.

        // Prepares the path_trace for the response and inserts the client node.
        let mut response_path_trace = flood_request.path_trace.clone();
        response_path_trace.push((self.id, NodeType::Client)); // Appends the client's own node and type to the path trace for the response.

        // Creates the FloodResponse.
        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id, // Carries over the flood ID from the request.
            path_trace: response_path_trace.clone(), // Sets the path trace for the response.
        };

        // Creates the response packet.
        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response.clone()), // Sets packet type to FloodResponse.
            routing_header: SourceRoutingHeader {
                hop_index: 1,
                // Reverses the path_trace to return to the sender.
                hops: response_path_trace.iter().rev().map(|(id, _)| *id).collect(), // Reverses the received path trace to create the return path.
            },
            session_id: packet.session_id, // Carries over the session ID from the request.
        };

        info!("Client {}: Sending FloodResponse, response packet: {:?}", self.id, response_packet); // Logged before sending a FloodResponse packet back to the initiator of the FloodRequest. Shows the recipient and the content of the response packet.

        // Sends the FloodResponse to the sender.
        let next_node = response_packet.routing_header.hops[1]; // Gets the next hop from the response packet's routing header.
        self.send_packet_and_notify(response_packet, next_node); // Sends the FloodResponse packet to the next hop.
    }
}
