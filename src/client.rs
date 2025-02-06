use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
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

        Self {
            id,
            sim_controller_send,
            sim_controller_recv,
            packet_recv,
            packet_send,
            client_type,
            message_storage: HashMap::new(),
            topology: HashSet::new(),
            node_types: HashMap::new(),
        }
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
                                // Add logic to actually remove the sender (if needed for Client)
                            }
                            ClientCommand::AddSender(nodeId, packet_sender) => {
                                log::info!("Client {}: Adding sender: {} -> {:?}",self.id, nodeId, packet_sender);
                                // Add logic to add the new sender.
                                self.packet_send.insert(nodeId, packet_sender);
                            }
                            ClientCommand::ServerType(nodeId) => {
                                log::info!("Client {}: Requesting ServerType from: {}", self.id, nodeId);
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
                        }
                    }
                },

            }
        }
    }

    fn handle_packet(&mut self, packet: Packet) {
        log::info!("Client {}: Received packet: {:?}", self.id, packet); // Log the received packet

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
                log::info!("Client {}: Received FloodRequest: {:?}", self.id, flood_request);
            }
            PacketType::Ack(session_id) => {
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
        // Send flood_request to the neighbour nodes
        let flood_request = FloodRequest {
            flood_id: generate_unique_id(),
            initiator_id: self.id,
            path_trace: Vec::new(),
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
}