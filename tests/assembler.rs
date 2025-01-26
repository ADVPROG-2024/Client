use std::collections::HashMap;
use std::thread;
use std::time::Duration;
use crossbeam_channel::unbounded;
use dronegowski_utils::functions::{assembler, deserialize_message, simple_log};
use dronegowski_utils::hosts::{ClientEvent, CustomEnum, CustomStruct, TestMessage};
use wg_2024::network::SourceRoutingHeader;
use wg_2024::packet::{Fragment, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use client::DronegowskiClient;

#[cfg(test)]
mod tests {
    use dronegowski_utils::functions::assembler;
    use super::*;
    use wg_2024::packet::Fragment;
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use std::collections::HashMap;
    use crossbeam_channel::unbounded;
    use dronegowski_utils::functions::simple_log;
    use dronegowski_utils::hosts::{ClientCommand, ClientEvent};
    use wg_2024::packet::PacketType;
    use client::DronegowskiClient;

    #[test]
    fn test_packet_received() {
        // Logger di simplelog
        simple_log();

        // Creazione dei canali
        let (sim_controller_send, sim_controller_recv) = unbounded::<ClientEvent>();
        let (send_controller, controller_recv) = unbounded::<ClientCommand>();
        let (packet_send, packet_recv) = unbounded::<Packet>();

        // Mappa dei vicini (drone collegati)
        let (neighbor_send, neighbor_recv) = unbounded();
        let mut senders = HashMap::new();
        senders.insert(2, neighbor_send); // Drone 2 come vicino

        // Creazione del client
        let mut client = DronegowskiClient::new(
            1, // ID del client
            sim_controller_send,
            controller_recv,
            packet_recv.clone(),
            senders,
        );

        let fragment1 = Packet { routing_header: SourceRoutingHeader { hop_index: 0, hops: vec![1, 2, 3] }, session_id: 42, pack_type: MsgFragment(Fragment { fragment_index: 1, total_n_fragments: 2, length: 43, data: [0, 0, 0, 0, 31, 0, 0, 0, 0, 0, 0, 0, 81, 117, 101, 115, 116, 111, 32, 195, 168, 32, 117, 110, 32, 109, 101, 115, 115, 97, 103, 103, 105, 111, 32, 100, 105, 32, 116, 101, 115, 116, 33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] }) };

        let fragment2 = Packet {
            pack_type: MsgFragment(Fragment {
                fragment_index: 0,
                total_n_fragments: 2,
                length: 12,
                data: [2, 0, 0, 0, 234, 0, 0, 0, 0, 0, 0, 0, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3],
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![1, 2],
            },
            session_id: 42,
        };

        // Avvia il client in un thread separato
        let mut handles = Vec::new();
        handles.push(thread::spawn(move || {
            client.run();
        }));

        // Invia i pacchetti
        packet_send.send(fragment1).unwrap();
        packet_send.send(fragment2).unwrap();

        // Attendi che il pacchetto venga ricevuto
        match sim_controller_recv.recv_timeout(Duration::from_secs(5)) {
            Ok(received_event) => {
                // Esegui pattern matching per estrarre il pacchetto
                match received_event {
                    ClientEvent::MessageReceived(received_packet) => {
                        println!("Packet {:?} successfully received by the node", received_packet);
                        assert_eq!(received_packet, TestMessage::Vector(vec![1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 0, 0, 0, 0, 31, 0, 0, 0, 0, 0, 0, 0, 81, 117, 101, 115, 116, 111, 32, 195, 168, 32, 117, 110, 32, 109, 101, 115, 115, 97, 103, 103, 105, 111, 32, 100, 105, 32, 116, 101, 115, 116, 33, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]))
                    }
                    _ => panic!("Unexpected event type received."),
                }
            }
            Err(_) => panic!("Timeout: No packet received."),
        }
    }

    #[test]
    fn test_assembler_correct_order() {
        let mut entry = vec![];
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 2,
            length: 128,
            data: [1; 128],
        };
        let fragment1 = Fragment {
            fragment_index: 1,
            total_n_fragments: 2,
            length: 128,
            data: [2; 128],
        };

        assembler(&mut entry, &fragment0);
        assembler(&mut entry, &fragment1);

        let expected_data: Vec<u8> = vec![1; 128].into_iter().chain(vec![2; 128].into_iter()).collect();
        assert_eq!(entry, expected_data, "I dati assemblati non corrispondono al risultato atteso");
    }

    #[test]
    fn test_assembler_out_of_order() {
        let mut entry = vec![];
        let fragment1 = Fragment {
            fragment_index: 1,
            total_n_fragments: 2,
            length: 128,
            data: [2; 128],
        };
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 2,
            length: 128,
            data: [1; 128],
        };

        assembler(&mut entry, &fragment1);
        assembler(&mut entry, &fragment0);

        let expected_data: Vec<u8> = vec![1; 128].into_iter().chain(vec![2; 128].into_iter()).collect();
        assert_eq!(entry, expected_data, "I dati assemblati non corrispondono al risultato atteso quando i frammenti sono fuori ordine");
    }
}


#[test]
fn test_message_fragmentation_and_reassembly() {
    use wg_2024::packet::{Packet, PacketType, Fragment};
    use dronegowski_utils::functions::{fragment_message, assembler, deserialize_message};
    use serde::{Serialize, Deserialize};
    use bincode;


    let test_cases = vec![
        TestMessage::Text(String::from("Questo è un messaggio di test!")),
        TestMessage::Number(123456u32), // Numero intero
        TestMessage::Vector(vec![1, 2, 3, 4, 5]), // Vettore di numeri
        TestMessage::Struct(CustomStruct {
            id: 42,
            name: String::from("Esempio"),
            data: vec![10, 20, 30],
        }),
        TestMessage::Enum(CustomEnum::Variant1(String::from("Test Enum"))),
        TestMessage::Enum(CustomEnum::Variant2 { id: 1, value: 3.14 }),
    ];

    let session_id = 42;
    let hops = vec![1, 2, 3]; // Percorso del messaggio

    for (index, message) in test_cases.into_iter().enumerate() {
        println!("Esecuzione test case {}: {:?}", index + 1, message);

        // Fase 1: Frammentazione
        let packets = fragment_message(&message, hops.clone(), session_id);

        assert!(!packets.is_empty());

        // Calcolo della dimensione del messaggio serializzato
        let serialized_message = bincode::serialize(&message).expect("Serialization failed");
        let mut reassembled_message = vec![0u8; serialized_message.len()];

        // Fase 2: Ricostruzione
        for packet in &packets {
            println!("{:?}", packet);
            if let PacketType::MsgFragment(fragment) = &packet.pack_type {
                assembler(&mut reassembled_message, fragment);
            }
        }

        // Fase 3: Deserializzazione e verifica
        let deserialized_message =
            deserialize_message::<TestMessage>(&reassembled_message).expect("Failed to deserialize message");

        assert_eq!(
            message, deserialized_message,
            "Il messaggio ricostruito non corrisponde a quello originale."
        );

        println!(
            "Messaggio ricostruito con successo per il test case {}: {:?}",
            index + 1,
            deserialized_message
        );
    }
}

#[test]
fn test_deserialize_message() {
    let mut entry = vec![2; 128];
    assembler(&mut entry, &Fragment { fragment_index: 0, total_n_fragments: 2, length: 128, data: [1; 128] });
    let result: Result<TestMessage, _> = deserialize_message(&entry);
    assert!(result.is_err(), "Dati non validi dovrebbero fallire");
}


#[test]
fn test_out_of_order_fragments() {
    let message = TestMessage::Vector(vec![1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7,1,3,4,5,6,7]);

    // Calcolo della dimensione del messaggio serializzato
    let serialized_message = bincode::serialize(&message).expect("Serialization failed");
    let mut reassembled_message = vec![0u8; 255];

    let packet1 = Packet { routing_header: SourceRoutingHeader { hop_index: 0, hops: vec![1, 2, 3] }, session_id: 42, pack_type: MsgFragment(Fragment { fragment_index: 0, total_n_fragments: 2, length: 128, data: [2, 0, 0, 0, 234, 0, 0, 0, 0, 0, 0, 0, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3] }) };
    let packet2 = Packet { routing_header: SourceRoutingHeader { hop_index: 0, hops: vec![1, 2, 3] }, session_id: 42, pack_type: MsgFragment(Fragment { fragment_index: 1, total_n_fragments: 2, length: 118, data: [4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 1, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] }) };

    if let MsgFragment(fragment) = &packet1.pack_type {
        assembler(&mut reassembled_message, fragment);
    }
    if let MsgFragment(fragment) = &packet2.pack_type {
        assembler(&mut reassembled_message, fragment);
    }

    // Fase 3: Deserializzazione e verifica
    let deserialized_message =
        deserialize_message(&reassembled_message).expect("Failed to deserialize message");

    assert_eq!(
        message, deserialized_message,
        "Il messaggio ricostruito non corrisponde a quello originale."
    );
}


