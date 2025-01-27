#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::thread;
    use std::thread::sleep;
    use std::time::Duration;
    use crossbeam_channel::unbounded;
    use dronegowski_utils::functions::simple_log;
    use dronegowski_utils::hosts::{ClientCommand, ClientEvent, TestMessage};
    use wg_2024::packet::{FloodResponse, NodeType, Packet, PacketType};
    use client::DronegowskiClient;

    #[test]
    fn server_type() {
        // Logger di simplelog
        simple_log();

        // Creazione dei canali
        let (sim_controller_send, sim_controller_recv) = unbounded::<ClientEvent>();
        let (send_controller, controller_recv) = unbounded::<ClientCommand>();
        let (packet_send, packet_recv) = unbounded::<Packet>();

        // Mappa dei vicini (drone collegati)
        let (neighbor_send, neighbor_recv) = unbounded();
        let mut senders = HashMap::new();
        senders.insert(4, neighbor_send); // Drone 4 come vicino

        // Creazione del client
        let mut client = DronegowskiClient::new(
            1, // ID del client
            sim_controller_send,
            controller_recv,
            packet_recv.clone(),
            senders,
        );

        let packet1 = Packet {
            routing_header: Default::default(),
            session_id: 0,
            pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Client), (4, NodeType::Drone), (12, NodeType::Server)] }),
        };

        let packet2 = Packet {
            routing_header: Default::default(),
            session_id: 0,
            pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Client), (6, NodeType::Drone), (11, NodeType::Drone), (2, NodeType::Drone), (12, NodeType::Server)] }),
        };

        let packet3 = Packet {
            routing_header: Default::default(),
            session_id: 0,
            pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Client), (6, NodeType::Drone), (4, NodeType::Drone), (12, NodeType::Server)] }),
        };

        let mut handles = Vec::new();

        // Create and start the client thread
        handles.push(thread::spawn(move || {
            client.run();
        }));

        // Send the flood response packet to the client
        packet_send.send(packet3).unwrap();
        packet_send.send(packet2).unwrap();
        packet_send.send(packet1).unwrap();

        let _ = send_controller.send(ClientCommand::ServerType(12));

        // Attendi che il pacchetto venga ricevuto
        match sim_controller_recv.recv_timeout(Duration::from_secs(5)) {
            Ok(received_event) => {
                // Esegui pattern matching per estrarre il pacchetto
                match received_event {
                    ClientEvent::PacketSent(received_packet) => {
                        println!("Packet {:?} successfully received by the SC", received_packet);
                    }
                    _ => panic!("Unexpected event type received."),
                }
            }
            Err(_) => panic!("Timeout: No packet received."),
        }

    }


}