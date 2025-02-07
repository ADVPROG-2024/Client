use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use crossbeam_channel::{select, select_biased, Receiver, Sender};
use dronegowski_utils::functions::{assembler, deserialize_message, fragment_message, generate_unique_id};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{FloodRequest, FloodResponse, Fragment, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::Ack;
use dronegowski_utils::hosts::{ClientCommand, ClientEvent, ClientMessages, ClientType, TestMessage, ServerMessages}; // Import ServerMessages
use serde::Serialize;
use bincode;

pub struct DronegowskiClient {
    pub id: NodeId,
    pub sim_controller_send: Sender<ClientEvent>, //Channel used to send commands to the SC
    pub sim_controller_recv: Receiver<ClientCommand>, //Channel used to receive commands from the SC
    pub packet_recv: Receiver<Packet>,           //Channel used to receive packets from nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, //Map containing the sending channels of neighbour nodes
    pub client_type: ClientType,
    pub message_storage: HashMap<(usize, NodeId), (Vec<u8>, Vec<bool>)>, // Store for reassembling messages
    pub topology: HashSet<(NodeId, NodeId)>, // Edges of the graph
    pub node_types: HashMap<NodeId, NodeType>, // Node types (Client, Drone, Server)
}

impl DronegowskiClient {
    pub fn new(id: NodeId, sim_controller_send: Sender<ClientEvent>, sim_controller_recv: Receiver<ClientCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, client_type: ClientType) -> Self {
        log::info!(
            "Client {} Created",
            id
        );

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
            select_biased!{
                recv(self.packet_recv) -> packet_res => {
                    if let Ok(packet) = packet_res {
                        self.handle_packet(packet);
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    if let Ok(command) = command_res {
                        log::info!("Client {}: Received ClientCommand: {:?}", self.id, command); // Log the received command

                        match command {
                            ClientCommand::RemoveSender(nodeId) => {
                                log::info!("Client {}: Removing sender: {}", self.id, nodeId);
                                self.remove_neighbor(&nodeId);
                                self.server_discovery();
                            }
                            ClientCommand::AddSender(nodeId, packet_sender) => {
                                log::info!("Client {}: Adding sender: {} -> {:?}",self.id, nodeId, packet_sender);
                                self.add_neighbor(nodeId, packet_sender);
                                self.server_discovery();
                            }
                            ClientCommand::ServerType(nodeId) => {
                                log::info!("Client {}: Requesting ServerType from Server: {}", self.id, nodeId);
                                self.ask_server_type(&nodeId);
                            }
                            ClientCommand::FilesList(nodeId) => {
                                log::info!("Client {}: Requesting FilesList from: {}", self.id, nodeId);
                                self.request_file_list(&nodeId);
                            }
                            ClientCommand::File(nodeId, fileId) => {
                                log::info!("Client {}: Requesting File (ID: {}) from: {}",self.id, fileId, nodeId);
                                self.request_file(&nodeId, fileId);
                            }
                            ClientCommand::Media(nodeId, mediaId) => {
                                log::info!("Client {}: Requesting Media (ID: {}) from: {}", self.id, mediaId, nodeId);
                                self.request_media(&nodeId, mediaId);
                            }
                            ClientCommand::ClientList(nodeId) => {
                                log::info!("Client {}: Requesting ClientList from: {}", self.id, nodeId);
                                self.request_client_list(&nodeId);
                            }
                            ClientCommand::RegistrationToChat(nodeId) => {
                                log::info!("Client {}: Requesting RegistrationToChat from: {}", self.id, nodeId);
                                self.register_with_server(&nodeId);
                            }
                            ClientCommand::MessageFor(nodeId, clientId, message) => {
                                log::info!("Client {}: Sending message to client {} via server {}: {}", self.id, clientId, nodeId, message);
                                self.send_message(&nodeId, clientId, message);
                            }
                            ClientCommand::RequestNetworkDiscovery => {
                                log::info!("Client {}: Requesting Network Discovery", self.id);
                                self.server_discovery();
                            }
                        }
                    }
                },

            }
        }
    }

    fn handle_packet(&mut self, packet: Packet) {
        log::info!("Client {}: Received packet: {:?}", self.id, packet);

        match packet.pack_type {
            PacketType::MsgFragment(fragment) => {
                if let Some(src_id) = packet.routing_header.source() {
                    log::info!("Client {}: Received MsgFragment from: {}, Session ID: {}, Fragment Index: {}, Total Fragments: {}",
                        self.id, src_id, packet.session_id, fragment.fragment_index, fragment.total_n_fragments);

                    let entry = self.message_storage
                        .entry((packet.session_id as usize, src_id))
                        .or_insert_with(|| {
                            log::info!("Client {}: Initializing message storage for session {} from {}", self.id, packet.session_id, src_id);
                            (
                                Vec::<u8>::with_capacity((fragment.total_n_fragments * 128) as usize),
                                vec![false; fragment.total_n_fragments as usize],
                            )
                        });

                    let (message_data, fragments_received) = entry;
                    assembler(message_data, &fragment);

                    let index = fragment.fragment_index as usize;
                    if index < fragments_received.len() {
                        fragments_received[index] = true;
                    }

                    if fragments_received.iter().all(|&received| received) {
                        let deserialized_message: Result<TestMessage, _> = deserialize_message::<TestMessage>(&message_data);

                        match deserialized_message {
                            Ok(res) => {
                                log::info!("Client {}: Message from session {} from {} fully reassembled: {:?}", self.id, packet.session_id, src_id, res);

                                // --- Handling ServerMessages ---
                                if let TestMessage::WebServerMessages(client_message) = res {
                                    match client_message{
                                        ClientMessages::ServerType => {}
                                        ClientMessages::FilesList => {}
                                        ClientMessages::File(_) => {}
                                        ClientMessages::Media(_) => {}
                                        ClientMessages::RegistrationToChat => {}
                                        ClientMessages::ClientList => {}
                                        ClientMessages::MessageFor(_, _) => {}
                                        ClientMessages::ServerMessages(server_message) => {
                                            match server_message {
                                                ServerMessages::ServerType(server_type) => {
                                                    log::info!("Client {}: Received ServerType: {:?}", self.id, server_type);
                                                    let _ = self.sim_controller_send.send(ClientEvent::ServerTypeReceived(src_id, server_type));
                                                }
                                                ServerMessages::ClientList(clients) => {
                                                    log::info!("Client {}: Received ClientList: {:?}", self.id, clients);
                                                    let _ = self.sim_controller_send.send(ClientEvent::ClientListReceived(src_id, clients));
                                                }
                                                ServerMessages::FilesList(files) => {
                                                    log::info!("Client {}: Received FilesList: {:?}", self.id, files);
                                                    let _ = self.sim_controller_send.send(ClientEvent::FilesListReceived(src_id, files));
                                                }
                                                ServerMessages::File(file_data) => {
                                                    log::info!("Client {}: Received File data (size: {} bytes)", self.id, file_data.len());
                                                    let _ = self.sim_controller_send.send(ClientEvent::FileReceived(src_id, file_data));
                                                }
                                                ServerMessages::Media(media_data) => {
                                                    log::info!("Client {}: Received Media data (size: {} bytes)", self.id, media_data.len());
                                                    let _ = self.sim_controller_send.send(ClientEvent::MediaReceived(src_id, media_data));
                                                }
                                                ServerMessages::MessageFrom(from_id, message) => {
                                                    log::info!("Client {}: Received MessageFrom: {} from {}", self.id, message, from_id);
                                                    let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived(src_id, from_id, message));
                                                }
                                                ServerMessages::RegistrationOk => {
                                                    log::info!("Client {}: Received RegistrationOk", self.id);
                                                    let _ = self.sim_controller_send.send(ClientEvent::RegistrationOk(src_id));
                                                }
                                                ServerMessages::RegistrationError(error) => {
                                                    log::info!("Client {}: Received RegistrationError, cause: {}", self.id, error);
                                                    let _ = self.sim_controller_send.send(ClientEvent::RegistrationError(src_id));
                                                }
                                                _ => {}
                                            }
                                        }
                                    }

                                } else {
                                    // Handle other TestMessage variants (if any)
                                    let _ = self.sim_controller_send.send(ClientEvent::MessageReceived(res));
                                }
                            }
                            Err(e) => {
                                log::error!("Client {}: Error deserializing message from session {} from {}: {:?}", self.id, packet.session_id, src_id, e);
                            }
                        }

                        //Clean the storage.
                        self.message_storage.remove(&(packet.session_id as usize, src_id));

                    } else {
                        let percentage = (fragments_received.iter().filter(|&&received| received).count() * 100)
                            / fragment.total_n_fragments as usize;
                        log::info!(
                            "Client {}: Received fragment {}/{} for session {} from {}.  {}% complete.",
                            self.id,
                            fragment.fragment_index + 1,
                            fragment.total_n_fragments,
                            packet.session_id,
                            src_id,
                            percentage
                        );
                    }
                }
            }
            PacketType::FloodResponse(flood_response) => {
                log::info!("Client {}: Received FloodResponse: {:?}", self.id, flood_response);
                self.update_graph(flood_response.path_trace);
            }
            PacketType::FloodRequest(flood_request) => {
                self.handle_flood_request(flood_request, packet.clone());
            },
            Ack(session_id) => {
                log::info!("Client {}: Received Ack for session: {}", self.id, session_id);
                // Handle ACK (if you are using ACKs)
            }
            PacketType::Nack(_) => {}
        }
    }

    pub fn switch_client_type(&mut self) {
        log::info!(
            "Client Type Switched"
        );
        if matches!(self.client_type, ClientType::ChatClients) {
            self.client_type = ClientType::WebBrowsers;
        } else {
            self.client_type = ClientType::ChatClients;
        }
    }

    pub fn server_discovery(&self) {

        let mut path_trace = Vec::new();
        path_trace.push((self.id, NodeType::Client));

        // Send flood_request to the neighbour nodes
        let flood_request = FloodRequest {
            flood_id: generate_unique_id(),
            initiator_id: self.id,
            path_trace,
        };

        for (node_id, sender) in &self.packet_send {
            log::info!("Inviando FloodRequest al nodo {}", node_id);
            let _ = sender.send(Packet {
                pack_type: PacketType::FloodRequest(flood_request.clone()),
                routing_header: SourceRoutingHeader {
                    hop_index: 0,
                    hops: vec![self.id, *node_id],
                },
                session_id: flood_request.flood_id,
            });
        }
    }

    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        log::info!("Aggiornamento del grafo con i dati ricevuti: {:?}", path_trace);
        for i in 0..path_trace.len() - 1 {
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b));
            self.topology.insert((node_b, node_a)); // Grafo bidirezionale
        }

        for (node_id, node_type) in path_trace {
            self.node_types.insert(node_id, node_type);
        }

    }

    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        use std::collections::VecDeque;

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut predecessors = HashMap::new();

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current) = queue.pop_front() {
            if current == *target_server {
                let mut path = vec![current];
                while let Some(&pred) = predecessors.get(&path[0]) {
                    path.insert(0, pred);
                }
                return Some(path);
            }

            for &(node_a, node_b) in &self.topology {
                if node_a == current && !visited.contains(&node_b) {
                    visited.insert(node_b);
                    queue.push_back(node_b);
                    predecessors.insert(node_b, current);
                }
            }
        }

        None
    }

    pub fn register_with_server(&self, server_id: &NodeId) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::RegistrationToChat);
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn request_client_list(&self,  server_id: &NodeId) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::ClientList);
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn send_message(&self,  server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::MessageFor(target_id, message_to_client));
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn request_file_list(&self, server_id: &NodeId) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::FilesList);
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn request_file(&self, server_id: &NodeId, file_id: u64) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::File(file_id));
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn request_media(&self, server_id: &NodeId, file_id: u64) {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::Media(file_id));
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    pub fn ask_server_type(&self, server_id: &NodeId) { // -> Server type {
        if let Some(path) = self.compute_route(server_id) {
            let message = TestMessage::WebServerMessages(ClientMessages::ServerType);
            let serialized_message = bincode::serialize(&message).expect("Serialization failed");
            log::info!("Path {:?}", path.clone());

            let res = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            if path.clone().len() > 1 {
                if let Some(sender) = self.packet_send.get(&path[1]) {
                    for packet in res {
                        log::info!("Inviando {:?} a nodo {:?}", packet, path[1]);
                        let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet.clone()));
                        sender.send(packet).expect("Invio pacchetto fallito");
                    }
                }
            }
        }
    }

    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        if let std::collections::hash_map::Entry::Vacant(e) = self.packet_send.entry(node_id) {
            e.insert(sender);
        } else {
            panic!("Sender for node {node_id} already stored in the map!");
        }
    }

    fn remove_neighbor(&mut self, node_id: &NodeId) {
        if self.packet_send.contains_key(node_id) {
            self.packet_send.remove(node_id);
        } else {
            panic!("the {} is not neighbour of the drone {}", node_id, self.id);
        }
    }

    fn handle_flood_request(&mut self, mut flood_request: FloodRequest, packet: Packet) {
        log::info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request);

        // 1. Add self to the path_trace.
        flood_request.path_trace.push((self.id, NodeType::Client));

        // 2. Create the FloodResponse.
        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: flood_request.path_trace.clone(),
        };

        // 3. Get the *source* of the FloodRequest.
        let source_id = packet.routing_header.source().expect("FloodRequest must have a source");

        // 4. Create the packet with the *reversed* path.
        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response),
            routing_header: SourceRoutingHeader {
                hop_index: 0, // Reset hop_index
                hops: flood_request.path_trace.iter().rev().map(|(id, _)| *id).collect(),
            },
            session_id: packet.session_id,
        };

        // 5. Send the response packet back to the source (with timeout).
        if let Some(sender) = self.packet_send.get(&source_id) {
            if let Err(_) = self.send_message_with_timeout(response_packet, source_id, Duration::from_millis(500)) {
                log::warn!("Client {}: Timeout sending FloodResponse to {}", self.id, source_id);
                self.server_discovery(); // Trigger network update on timeout
            } else {
                log::info!("Client {}: Sent FloodResponse back to {}", self.id, source_id);
                // Update the graph *after* successfully sending the response.
                self.update_graph(flood_request.path_trace);
            }
        } else {
            log::error!("Client {}: No sender found for node {}", self.id, source_id);
        }
    }

    fn send_message_with_timeout(&self, packet: Packet, recipient_id: NodeId, timeout: Duration) -> Result<(), ()> {
        if let Some(sender) = self.packet_send.get(&recipient_id) {
            match sender.send_timeout(packet.clone(), timeout) { // Use send_timeout
                Ok(()) => {
                    let _ = self.sim_controller_send.send(ClientEvent::PacketSent(packet));
                    Ok(())
                },
                Err(_) => {  //  The specific error type is SendTimeoutError
                    log::warn!("Client {}: Timeout sending packet to {}", self.id, recipient_id);
                    Err(()) // Indicate timeout
                }
            }
        } else {
            log::warn!("Client {}: No sender found for node {}", self.id, recipient_id);
            Err(()) // Indicate no sender
        }
    }

}