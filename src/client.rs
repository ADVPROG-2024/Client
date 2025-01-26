use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use crossbeam_channel::{select, select_biased, Receiver, Sender};
use dronegowski_utils::functions::{assembler, deserialize_message, fragment_message, generate_unique_id};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{FloodRequest, FloodResponse, Fragment, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::Ack;
use dronegowski_utils::hosts::{ClientCommand, ClientEvent, TestMessage};

#[derive(Clone, Debug)]
pub enum ClientType {
    WebBrowsers,
    ChatClients,
}

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
    pub fn new(id: NodeId, sim_controller_send: Sender<ClientEvent>, sim_controller_recv: Receiver<ClientCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>) -> Self {
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
            client_type: ClientType::ChatClients,
            message_storage: HashMap::new(),
            topology: HashSet::new(),
            node_types: HashMap::new(),
        }
    }

    pub fn run(&mut self) {
        loop {
            // log::info!("Client entering the run loop in state");
            select!{
                recv(self.packet_recv) -> packet_res => {
                    if let Ok(packet) = packet_res {
                        self.handle_packet(packet);
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    // if let Ok(command) = command_res {
                        log::info!("Ricevuto un Client Command");
                        self.ask_server_type(12);
                    // }
                },

            }
        }
    }

    fn handle_packet(&mut self, packet: Packet) {
        match packet.pack_type {
            PacketType::MsgFragment(fragment) => {
                if let Some(src_id) = packet.routing_header.source() {
                    // Recupera o inizializza lo storage del messaggio
                    let entry = self.message_storage
                        .entry((packet.session_id as usize, src_id))
                        .or_insert_with(|| {
                            (
                                Vec::<u8>::with_capacity((fragment.total_n_fragments * 128) as usize), // Dati del messaggio
                                vec![false; fragment.total_n_fragments as usize], // Frammenti ricevuti
                            )
                        });

                    let (message_data, fragments_received) = entry;

                    // Assembla il frammento
                    log::info!("{:?}", fragment);
                    assembler(message_data, &fragment);

                    // Segna il frammento come ricevuto
                    let index = fragment.fragment_index as usize; // Usa indici 0-based
                    if index < fragments_received.len() {
                        fragments_received[index] = true;
                    }

                    // Verifica se il messaggio è completo
                    if fragments_received.iter().all(|&received| received) {
                        let deserialized_message: Result<TestMessage, _> = deserialize_message::<TestMessage>(&message_data);

                        match deserialized_message {
                            Ok(res) => {
                                log::info!("Ricevuto frammento da sessione {} del nodo {}: frammento {}. Messaggio ricevuto al 100%", packet.session_id, src_id, fragment.fragment_index);
                                let _ = self.sim_controller_send.send(ClientEvent::MessageReceived(res));
                            }
                            Err(e) => {
                                log::info!("{:?}", e);
                            }
                        }
                    } else {
                        // Percentuale basata sui frammenti ricevuti
                        let percentuale = (fragments_received.iter().filter(|&&received| received).count() * 100)
                            / fragment.total_n_fragments as usize;
                        log::info!(
                        "Ricevuto frammento da sessione {} del nodo {}: frammento {}. Messaggio ricevuto al {}%",
                        packet.session_id, src_id, fragment.fragment_index, percentuale
                    );
                    }
                }
            }

            PacketType::FloodResponse(flood_response) => {
                self.update_graph(flood_response.path_trace);
            }
            _ => {}
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

    fn compute_route(&self, target_server: NodeId) -> Option<Vec<NodeId>> {
        use std::collections::VecDeque;

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut predecessors = HashMap::new();

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current) = queue.pop_front() {
            if current == target_server {
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

    pub fn register_with_server(&self) -> Result<(), String> {
        // Logica di registrazione
        Ok(())
    }

    pub fn request_client_list(&self) -> Result<(), String> {
        // Logica per richiedere la lista clienti
        Ok(())
    }

    pub fn send_message(&self, target_id: NodeId, message: &str) -> Result<(), String> {
        // Logica per inviare un messaggio
        Ok(())
    }

    pub fn request_file_list(&self) -> Result<(), String> {
        // Logica per ottenere la lista file
        Ok(())
    }

    pub fn request_file(&self, file_name: &str) -> Result<(), String> {
        // Logica per scaricare un file
        Ok(())
    }

    pub fn ask_server_type(&self, server_id: NodeId) { // -> Server type {
        // let path = self.compute_route(server_id);
        // // Frammentare il messaggio
        // let message = "ciao";
        // let res = fragment_message(&message, path.unwrap(), generate_unique_id());
        // for packet in res {
        //     log::info!("{:?}", packet)
        // }

        // Inserire i frammenti all'interno di pacchetti

        // Mandare tutti i frammenti al primo nodo del path


    }

    // pub fn registration_to_chat(&self, server_id: NodeId) -> Result {
    //     if self.ask_server_type(server_id) == Communication server {
    //         // Mandare un messaggio al server
    //         fragment("registration_to_chat")
    //     }
    // }
}


