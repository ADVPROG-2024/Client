use std::collections::HashMap;
use std::thread;
use std::thread::sleep;
use std::time::Duration;
use crossbeam_channel::unbounded;
use dronegowski_utils::functions::{assembler, deserialize_message, fragment_message, simple_log};
use wg_2024::packet::{FloodResponse, Fragment, NodeType, Packet, PacketType};
use client::{DronegowskiClient};
use dronegowski_utils::hosts::{ClientCommand, ClientEvent, ClientType, CustomEnum, CustomStruct, TestMessage};
use wg_2024::network::SourceRoutingHeader;
use wg_2024::packet::PacketType::MsgFragment;

#[test]
fn test_network_discovery_protocol() {
    // Logger di simplelog
    simple_log();

    // Creazione dei canali
    let (sim_controller_send, _sim_controller_recv) = unbounded::<ClientEvent>();
    let (_send_controller, controller_recv) = unbounded::<ClientCommand>();
    let (packet_send, packet_recv) = unbounded::<Packet>();

    // Mappa dei vicini (drone collegati)
    let (neighbor_send, _neighbor_recv) = unbounded();
    let mut senders = HashMap::new();
    senders.insert(2, neighbor_send); // Drone 2 come vicino

    // Creazione del client
    let mut client = DronegowskiClient::new(
        1, // ID del client
        sim_controller_send.clone(),
        controller_recv,
        packet_recv.clone(),
        senders,
        ClientType::ChatClients,
    );

    let packet1 = Packet {
        routing_header: Default::default(),
        session_id: 0,
        pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Server), (4, NodeType::Drone), (12, NodeType::Server)] }),
    };

    let packet2 = Packet {
        routing_header: Default::default(),
        session_id: 0,
        pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Server), (6, NodeType::Drone), (11, NodeType::Drone), (2, NodeType::Drone), (12, NodeType::Server)] }),
    };

    let packet3 = Packet {
        routing_header: Default::default(),
        session_id: 0,
        pack_type: PacketType::FloodResponse(FloodResponse { flood_id: 12, path_trace: vec![(1, NodeType::Server), (6, NodeType::Drone), (4, NodeType::Drone), (12, NodeType::Server)] }),
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

    sleep(Duration::new(2, 0));
    
    // sim_controller_send.send(ClientEvent::MessageReceived(Vec::from("ciao")));
    // client.ask_server_type(12);

    // Sleep for 5 seconds
    sleep(Duration::new(6, 0));
}



