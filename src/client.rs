use std::collections::{HashMap, HashSet, VecDeque};

use crossbeam_channel::{select_biased, Receiver, Sender};
use dronegowski_utils::functions::{
    assembler, deserialize_message, fragment_message, generate_unique_id,
};
use dronegowski_utils::hosts::{
    ClientCommand, ClientEvent, ClientMessages, ClientType, ServerMessages, TestMessage,
};
use log::{debug, error, info, warn};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{FloodRequest, FloodResponse, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::Ack;

/// `DronegowskiClient` rappresenta un client all'interno della simulazione.
/// Gestisce la comunicazione con il simulatore, l'invio e la ricezione di pacchetti,
/// e la logica specifica del client (chat o web browsing).
#[derive(Debug)]
pub struct DronegowskiClient {
    /// Identificativo univoco del client.
    pub id: NodeId,
    /// Canale per inviare eventi al controllore della simulazione.
    sim_controller_send: Sender<ClientEvent>,
    /// Canale per ricevere comandi dal controllore della simulazione.
    sim_controller_recv: Receiver<ClientCommand>,
    /// Canale per ricevere pacchetti dalla rete.
    packet_recv: Receiver<Packet>,
    /// Mappa che associa ad ogni nodo un canale per inviargli pacchetti.
    packet_send: HashMap<NodeId, Sender<Packet>>,
    /// Tipo di client (ChatClients o WebBrowsers).
    pub client_type: ClientType,
    /// Mappa che memorizza i frammenti di messaggio in arrivo per la riassemblaggio.
    /// La chiave è una tupla (session_id, mittente), il valore è una tupla (dati del messaggio, vettore di booleani che indica quali frammenti sono arrivati).
    message_storage: HashMap<(usize, NodeId), (Vec<u8>, Vec<bool>)>,
    /// Insieme di tuple (NodeId, NodeId) che rappresenta la topologia della rete vista dal client.
    topology: HashSet<(NodeId, NodeId)>,
    /// Mappa che associa ad ogni nodo il suo tipo (Client, Server, Intermediate).
    node_types: HashMap<NodeId, NodeType>,
}

impl DronegowskiClient {
    /// Crea un nuovo `DronegowskiClient`.
    ///
    /// # Argomenti
    ///
    /// * `id`: L'ID del client.
    /// * `sim_controller_send`: Il canale per inviare eventi al controllore della simulazione.
    /// * `sim_controller_recv`: Il canale per ricevere comandi dal controllore della simulazione.
    /// * `packet_recv`: Il canale per ricevere pacchetti.
    /// * `packet_send`: Una mappa di canali per inviare pacchetti ad altri nodi.
    /// * `client_type`: Il tipo di client.
    pub fn new(
        id: NodeId,
        sim_controller_send: Sender<ClientEvent>,
        sim_controller_recv: Receiver<ClientCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        client_type: ClientType,
    ) -> Self {
        let client = Self {
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

        // Esegue la scoperta iniziale dei server.
        client.server_discovery();
        client
    }

    /// Avvia il ciclo principale del client.
    ///
    /// Il client rimane in ascolto su due canali:
    /// - `packet_recv`: per la ricezione di pacchetti.
    /// - `sim_controller_recv`: per la ricezione di comandi dal simulatore.
    ///
    /// Utilizza `select_biased` per dare priorità alla ricezione dei pacchetti.
    pub fn run(&mut self) {
        loop {
            select_biased! {
                recv(self.packet_recv) -> packet_res => {
                    if let Ok(packet) = packet_res {
                        self.handle_packet(packet);
                    } else {
                        error!("Client {}: Errore nella ricezione del pacchetto", self.id);
                    }
                }
                recv(self.sim_controller_recv) -> command_res => {
                    if let Ok(command) = command_res {
                        self.handle_client_command(command);
                    } else {
                        error!("Client {}: Errore nella ricezione del comando dal simulatore", self.id);
                    }
                }
            }
        }
    }

    /// Gestisce un comando ricevuto dal controllore della simulazione.
    fn handle_client_command(&mut self, command: ClientCommand) {
        info!("Client {}: Ricevuto comando dal simulatore: {:?}", self.id, command);
        match command {
            ClientCommand::RemoveSender(node_id) => {
                // Rimuove un vicino e riesegue la scoperta dei server.
                self.remove_neighbor(&node_id);
                self.server_discovery();
            }
            ClientCommand::AddSender(node_id, packet_sender) => {
                // Aggiunge un vicino e riesegue la scoperta dei server.
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

    /// Gestisce un pacchetto ricevuto.
    fn handle_packet(&mut self, packet: Packet) {
        info!("Client {}: Ricevuto pacchetto: {:?}", self.id, packet);

        match packet.pack_type {
            PacketType::MsgFragment(_) => self.handle_message_fragment(packet),
            PacketType::FloodResponse(flood_response) => {
                info!(
                    "Client {}: Ricevuto FloodResponse: {:?}",
                    self.id,
                    flood_response
                );
                self.update_graph(flood_response.path_trace);
            }
            PacketType::FloodRequest(_) => self.handle_flood_request(packet),
            Ack(session_id) => {
                info!("Client {}: Ricevuto Ack per la sessione: {}", self.id, session_id);
            }
            PacketType::Nack(_) => {
                // I pacchetti Nack non vengono gestiti al momento. Potrebbe essere necessario implementarli per la gestione degli errori.
                debug!("Client {}: Ricevuto Nack (non gestito)", self.id);

            }
        }
    }


    /// Gestisce un frammento di messaggio ricevuto.
    fn handle_message_fragment(&mut self, packet: Packet) {
        let fragment = match packet.pack_type {
            PacketType::MsgFragment(f) => f,
            _ => {
                // Non dovrebbe mai succedere, dato che la funzione è chiamata solo per MsgFragment.
                error!("Client {}: handle_message_fragment chiamato con un tipo di pacchetto non MsgFragment", self.id);
                return;
            }
        };

        let src_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                warn!("Client {}: MsgFragment senza mittente", self.id);
                return;
            }
        };

        info!(
            "Client {}: Ricevuto MsgFragment da: {}, Sessione: {}, Indice: {}, Totale: {}",
            self.id,
            src_id,
            packet.session_id,
            fragment.fragment_index,
            fragment.total_n_fragments
        );

        // Logica per il riassemblaggio dei frammenti.
        let reassembled_data = {
            let key = (packet.session_id as usize, src_id);
            // Prende o inserisce una nuova entry nella mappa `message_storage`.
            let (message_data, fragments_received) = self
                .message_storage
                .entry(key)
                .or_insert_with(|| {
                    info!(
                        "Client {}: Inizializzazione dello storage per la sessione {} da {}",
                        self.id,
                        packet.session_id,
                        src_id
                    );
                    // Inizializza il vettore per i dati del messaggio e il vettore per tenere traccia dei frammenti ricevuti.
                    (
                        Vec::with_capacity((fragment.total_n_fragments * 128) as usize),
                        vec![false; fragment.total_n_fragments as usize],
                    )
                });

            // Assembla il frammento corrente.
            assembler(message_data, &fragment);
            // Segna il frammento come ricevuto.
            if (fragment.fragment_index as usize) < fragments_received.len() {
                fragments_received[fragment.fragment_index as usize] = true;
            } else {
                error!("Client {}: Indice del frammento {} fuori dai limiti per la sessione {} da {}", self.id, fragment.fragment_index, packet.session_id, src_id);
                return;
            }

            // Controlla se tutti i frammenti sono stati ricevuti.
            let all_fragments_received = fragments_received.iter().all(|&received| received);


            let percentage = (fragments_received.iter().filter(|&&r| r).count() * 100) / fragment.total_n_fragments as usize;
            info!(
                "Client {}: Frammento {}/{} per la sessione {} da {}. {}% completo.",
                self.id,
                fragment.fragment_index + 1,
                fragment.total_n_fragments,
                packet.session_id,
                src_id,
                percentage
            );


            if all_fragments_received {
                // Se tutti i frammenti sono stati ricevuti, restituisce i dati del messaggio.
                Some((packet.session_id, message_data.clone()))
            } else {
                // Altrimenti, restituisce None.
                None
            }
        };

        // Se il messaggio è stato riassemblato, lo processa.
        if let Some((session_id, message_data)) = reassembled_data {
            self.process_reassembled_message(session_id, src_id, &message_data);
            // Rimuove la entry dalla mappa `message_storage`.
            self.message_storage.remove(&(session_id as usize, src_id));
            info!("Client {}: Messaggio dalla sessione {} da {} rimosso dallo storage", self.id, session_id, src_id);
        }
    }

    /// Processa un messaggio riassemblato.
    fn process_reassembled_message(&mut self, session_id: u64, src_id: NodeId, message_data: &[u8]) {
        // Deserializza il messaggio.
        match deserialize_message::<TestMessage>(message_data) {
            Ok(TestMessage::WebServerMessages(client_message)) => {
                // Se il messaggio è un messaggio del web server, lo gestisce.
                self.handle_server_message(src_id, client_message);
            }
            Ok(deserialized_message) => {
                info!(
                    "Client {}: Messaggio dalla sessione {} da {} completamente riassemblato: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    deserialized_message
                );
                // Invia il messaggio ricevuto al controllore della simulazione.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::MessageReceived(deserialized_message));
            }
            Err(e) => {
                error!(
                    "Client {}: Errore nella deserializzazione della sessione {} da {}: {:?}",
                    self.id,
                    session_id,
                    src_id,
                    e
                );
            }
        }
    }

    /// Gestisce un messaggio ricevuto da un server.
    fn handle_server_message(&mut self, src_id: NodeId, client_message: ClientMessages) {
        match client_message {
            ClientMessages::ServerMessages(server_message) => match server_message {
                ServerMessages::ServerType(server_type) => {
                    info!("Client {}: Ricevuto ServerType: {:?}", self.id, server_type);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ServerTypeReceived(src_id, server_type));
                }
                ServerMessages::ClientList(clients) => {
                    info!("Client {}: Ricevuto ClientList: {:?}", self.id, clients);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::ClientListReceived(src_id, clients));
                }
                ServerMessages::FilesList(files) => {
                    info!("Client {}: Ricevuto FilesList: {:?}", self.id, files);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FilesListReceived(src_id, files));
                }
                ServerMessages::File(file_data) => {
                    info!(
                        "Client {}: Ricevuti dati del file (dimensione: {} bytes)",
                        self.id,
                        file_data.text.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::FileReceived(src_id, file_data.text));
                }
                ServerMessages::Media(media_data) => {
                    info!(
                        "Client {}: Ricevuti dati multimediali (dimensione: {} bytes)",
                        self.id,
                        media_data.len()
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::MediaReceived(src_id, media_data));
                }
                ServerMessages::MessageFrom(from_id, message) => {
                    info!(
                        "Client {}: Ricevuto MessageFrom: {} da {}",
                        self.id,
                        message,
                        from_id
                    );
                    let _ = self.sim_controller_send.send(ClientEvent::MessageFromReceived(
                        src_id, from_id, message,
                    ));
                }
                ServerMessages::RegistrationOk => {
                    info!("Client {}: Ricevuto RegistrationOk", self.id);
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationOk(src_id));
                }
                ServerMessages::RegistrationError(error) => {
                    info!(
                        "Client {}: Ricevuto RegistrationError, causa: {}",
                        self.id,
                        error
                    );
                    let _ = self
                        .sim_controller_send
                        .send(ClientEvent::RegistrationError(src_id));
                }
                other => {
                    debug!("Client {}: Ricevuto messaggio server non gestito: {:?}", self.id, other);
                }
            },
            other => {
                debug!("Client {}: Ricevuto messaggio client non gestito: {:?}", self.id, other)
            }
        }
    }


    /// Cambia il tipo di client (ChatClients <-> WebBrowsers).
    pub fn switch_client_type(&mut self) {
        info!("Client {}: Cambio del tipo di client", self.id);
        self.client_type = match self.client_type {
            ClientType::ChatClients => ClientType::WebBrowsers,
            ClientType::WebBrowsers => ClientType::ChatClients,
        };
        info!("Client {}: Nuovo tipo di client: {:?}", self.id, self.client_type);
    }

    /// Invia una richiesta di Flood per scoprire i server.
    pub fn server_discovery(&self) {
        info!("Client {}: Inizio della scoperta dei server", self.id);
        let flood_request = FloodRequest {
            flood_id: generate_unique_id(),
            initiator_id: self.id,
            path_trace: vec![(self.id, NodeType::Client)],
        };

        // Invia una richiesta di Flood a tutti i vicini.
        for (&node_id, _) in &self.packet_send {
            info!("Client {}: Invio di FloodRequest al nodo {}", self.id, node_id);
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

    /// Aggiorna la topologia della rete e i tipi di nodi in base al path_trace ricevuto.
    fn update_graph(&mut self, path_trace: Vec<(NodeId, NodeType)>) {
        info!("Client {}: Aggiornamento del grafo con: {:?}", self.id, path_trace);
        // Aggiunge gli archi al grafo (bidirezionale).
        for i in 0..path_trace.len() - 1 {
            let (node_a, _) = path_trace[i];
            let (node_b, _) = path_trace[i + 1];
            self.topology.insert((node_a, node_b));
            self.topology.insert((node_b, node_a));
        }
        debug!("Client {}: Topologia aggiornata: {:?}", self.id, self.topology);

        // Aggiorna i tipi di nodo.
        for (node_id, node_type) in path_trace {
            self.node_types.insert(node_id, node_type);
        }
        debug!("Client {}: Tipi di nodo aggiornati: {:?}", self.id, self.node_types);
    }

    /// Calcola un percorso dal client al server di destinazione utilizzando una BFS.
    fn compute_route(&self, target_server: &NodeId) -> Option<Vec<NodeId>> {
        info!("Client {}: Calcolo del percorso verso {}", self.id, target_server);
        info!("Client {}: Topologia corrente: {:?}", self.id, self.topology);

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        // Mappa per tenere traccia dei predecessori nel percorso.
        let mut predecessors: HashMap<NodeId, NodeId> = HashMap::new();

        queue.push_back(self.id);
        visited.insert(self.id);

        while let Some(current_node) = queue.pop_front() {
            debug!("Client {}: Nodo corrente nella BFS: {}", self.id, current_node);

            // Se il nodo corrente è il server di destinazione, ricostruisce il percorso e lo restituisce.
            if current_node == *target_server {
                debug!("Client {}: Server di destinazione {} trovato!", self.id, target_server);
                let mut path = Vec::new();
                let mut current = *target_server;
                // Ricostruisce il percorso a ritroso dai predecessori.
                while let Some(&prev) = predecessors.get(&current) {
                    path.push(current);
                    current = prev;
                }
                path.push(self.id); // Aggiunge il nodo di partenza (il client stesso).
                path.reverse(); // Inverte il percorso per ottenere l'ordine corretto.
                info!("Client {}: Percorso trovato: {:?}", self.id, path);
                return Some(path);
            }

            // Itera sui vicini in base alla topologia *bidirezionale*.
            for &(node_a, node_b) in &self.topology {
                // Controlla i vicini in entrambe le direzioni.
                if node_a == current_node && !visited.contains(&node_b) {
                    debug!("Client {}: Esplorazione del vicino: {} di {}", self.id, node_b, node_a);
                    visited.insert(node_b);
                    queue.push_back(node_b);
                    predecessors.insert(node_b, node_a); // Memorizza il predecessore.
                } else if node_b == current_node && !visited.contains(&node_a) {
                    debug!("Client {}: Esplorazione del vicino: {} di {}", self.id, node_a, node_b);
                    visited.insert(node_a);
                    queue.push_back(node_a);
                    predecessors.insert(node_a, node_b); // Memorizza il predecessore.
                }
            }
        }

        // Se non viene trovato alcun percorso, restituisce None.
        warn!("Client {}: Nessun percorso trovato verso {}", self.id, target_server);
        None
    }

    /// Invia un messaggio `ClientMessages` a un server.
    fn send_client_message_to_server(
        &self,
        server_id: &NodeId,
        client_message: ClientMessages,
    ) {
        let message = TestMessage::WebServerMessages(client_message);
        self.send_message_to_node(server_id, message);
    }

    /// Invia una richiesta di registrazione alla chat a un server.
    pub fn register_with_server(&self, server_id: &NodeId) {
        info!("Client {}: Invio della richiesta di registrazione alla chat al server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::RegistrationToChat);
    }

    /// Richiede la lista dei client connessi a un server.
    pub fn request_client_list(&self, server_id: &NodeId) {
        info!("Client {}: Richiesta della lista dei client al server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::ClientList);
    }

    /// Invia un messaggio a un altro client tramite un server.
    pub fn send_message(&self, server_id: &NodeId, target_id: NodeId, message_to_client: String) {
        info!("Client {}: Invio del messaggio \"{}\" al client {} tramite il server {}", self.id, message_to_client, target_id, server_id);
        self.send_client_message_to_server(
            server_id,
            ClientMessages::MessageFor(target_id, message_to_client),
        );
    }

    /// Richiede la lista dei file disponibili su un server.
    pub fn request_file_list(&self, server_id: &NodeId) {
        info!("Client {}: Richiesta della lista dei file al server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::FilesList);
    }

    /// Richiede un file specifico a un server.
    pub fn request_file(&self, server_id: &NodeId, file_id: u64) {
        info!("Client {}: Richiesta del file {} al server {}", self.id, file_id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::File(file_id));
    }

    /// Richiede un media specifico a un server.
    pub fn request_media(&self, server_id: &NodeId, file_id: u64) {
        info!("Client {}: Richiesta del media {} al server {}", self.id, file_id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::Media(file_id));
    }

    /// Richiede il tipo di un server.
    pub fn request_server_type(&self, server_id: &NodeId) {
        info!("Client {}: Richiesta del tipo di server al server {}", self.id, server_id);
        self.send_client_message_to_server(server_id, ClientMessages::ServerType);
    }


    /// Invia un messaggio (`TestMessage`) a un nodo specifico.
    /// Il messaggio viene frammentato e inviato come una serie di pacchetti `MsgFragment`.
    fn send_message_to_node(&self, target_id: &NodeId, message: TestMessage) {
        // Calcola il percorso verso il nodo di destinazione.
        if let Some(path) = self.compute_route(target_id) {
            info!("Client {}: Percorso trovato correttamente", self.id);
            // Serializza il messaggio.
            let serialized_message = bincode::serialize(&message).expect("Serializzazione fallita");
            // Frammenta il messaggio.
            let fragments = fragment_message(&serialized_message, path.clone(), generate_unique_id());

            // Invia i frammenti al primo hop del percorso.
            if let (Some(next_hop), true) = (path.get(1), path.len() > 1) {
                if let Some(_) = self.packet_send.get(next_hop) {
                    for packet in fragments {
                        info!("Client {}: Invio del pacchetto al next hop {}", self.id, *next_hop);
                        self.send_packet_and_notify(packet, *next_hop);
                    }
                } else {
                    error!("Client {}: Nessun sender per il next hop {}", self.id, next_hop);
                }
            } else {
                error!("Client {}: Percorso non valido verso {}", self.id, target_id);
            }
        } else {
            warn!("Client {}: Nessun percorso verso {}", self.id, target_id);
        }
    }

    /// Invia un pacchetto a un destinatario e notifica il controllore della simulazione.
    fn send_packet_and_notify(&self, packet: Packet, recipient_id: NodeId) {
        if let Some(sender) = self.packet_send.get(&recipient_id) {
            if let Err(e) = sender.send(packet.clone()) {
                error!(
                    "Client {}: Errore nell'invio del pacchetto a {}: {:?}",
                    self.id,
                    recipient_id,
                    e
                );
            } else {
                info!(
                    "Client {}: Pacchetto inviato a {}: deve arrivare a {}",
                    self.id,
                    recipient_id,
                    packet.routing_header.hops.last().unwrap(),
                );

                // Notifica il controllore della simulazione dell'invio del pacchetto.
                let _ = self
                    .sim_controller_send
                    .send(ClientEvent::PacketSent(packet));
            }
        } else {
            error!("Client {}: Nessun sender per il nodo {}", self.id, recipient_id);
        }
    }

    /// Aggiunge un vicino alla mappa dei sender.
    fn add_neighbor(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        info!("Client {}: Aggiunta del vicino {}", self.id, node_id);
        if self.packet_send.insert(node_id, sender).is_some() {
            warn!("Client {}: Sostituito sender esistente per il nodo {}", self.id, node_id);
        }
    }

    /// Rimuove un vicino dalla mappa dei sender.
    fn remove_neighbor(&mut self, node_id: &NodeId) {
        info!("Client {}: Rimozione del vicino {}", self.id, node_id);
        if self.packet_send.remove(node_id).is_none() {
            warn!("Client {}: Il nodo {} non era un vicino.", self.id, node_id);
        }
    }

    /// Gestisce un `FloodRequest` ricevuto.
    fn handle_flood_request(&mut self, packet: Packet) {
        // Estrae il FloodRequest dal pacchetto.
        let flood_request = match packet.pack_type {
            PacketType::FloodRequest(req) => req,
            _ => {
                // Non dovrebbe mai succedere.
                error!("Client {}: handle_flood_request chiamato con un tipo di pacchetto non FloodRequest", self.id);
                return;
            }
        };

        info!("Client {}: Ricevuto FloodRequest: {:?}", self.id, flood_request);

        // Ricava l'ID del mittente.
        let source_id = match packet.routing_header.source() {
            Some(id) => id,
            None => {
                warn!("Client {}: FloodRequest senza mittente", self.id);
                return;
            }
        };

        // Aggiorna il grafo con le informazioni del path_trace.
        self.update_graph(flood_request.path_trace.clone());

        // Prepara il path_trace per la risposta e inserisce il nodo del client.
        let mut response_path_trace = flood_request.path_trace.clone();
        response_path_trace.push((self.id, NodeType::Client));

        // Crea il FloodResponse.
        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: response_path_trace,
        };

        // Crea il pacchetto di risposta.
        let response_packet = Packet {
            pack_type: PacketType::FloodResponse(flood_response.clone()),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                // Inverte il path_trace per tornare al mittente.
                hops: flood_request.path_trace.iter().rev().map(|(id, _)| *id).collect(),
            },
            session_id: packet.session_id,
        };

        info!("Client {}: Invio di FloodResponse a {}, response packet: {:?}", self.id, source_id, response_packet);

        // Invia il FloodResponse al mittente.
        let next_node = response_packet.routing_header.hops[0];
        info!("Client {}: Invio di FloodResponse tramite {}", self.id, next_node);
        self.send_packet_and_notify(response_packet, next_node);
    }

    // Alternativa alla send_packet_and_notify con timeout, non utilizzata nel codice
    // /// Invia un pacchetto con un timeout.
    // fn send_message_with_timeout(
    //     &self,
    //     packet: Packet,
    //     recipient_id: NodeId,
    //     timeout: Duration,
    // ) -> Result<(), ()> {
    //     match self.packet_send.get(&recipient_id) {
    //         Some(sender) => match sender.send_timeout(packet.clone(), timeout) {
    //             Ok(()) => {
    //                 let _ = self
    //                     .sim_controller_send
    //                     .send(ClientEvent::PacketSent(packet));
    //                 Ok(())
    //             }
    //             Err(_) => {
    //                 log::warn!(
    //                     "Client {}: Timeout nell'invio del pacchetto a {}",
    //                     self.id,
    //                     recipient_id
    //                 );
    //                 Err(())
    //             }
    //         },
    //         None => {
    //             log::warn!("Client {}: Nessun sender per il nodo {}", self.id, recipient_id);
    //             Err(())
    //         }
    //     }
    // }
}