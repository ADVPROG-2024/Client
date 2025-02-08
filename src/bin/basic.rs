use std::collections::HashMap;
use std::thread;
use crossbeam_channel::unbounded;
use wg_2024::packet::{Packet};
use client::{DronegowskiClient};
use dronegowski_utils::hosts::{ClientCommand, ClientEvent, ClientType};
use dronegowski_utils::functions::simple_log;

fn main() {

    // Logger di simplelog
    simple_log();

    // Creazione dei canali
    let (sim_controller_send, _) = unbounded::<ClientEvent>();
    let (_, controller_recv) = unbounded::<ClientCommand>();
    let (_, packet_recv) = unbounded::<Packet>();

    // Mappa dei vicini (drone collegati)
    let (neighbor_send, _) = unbounded();
    let mut senders = HashMap::new();
    senders.insert(2, neighbor_send); // Drone 2 come vicino

    // Creazione del client
    let mut client = DronegowskiClient::new(
        1, // ID del client
        sim_controller_send,
        controller_recv,
        packet_recv.clone(),
        senders,
        ClientType::ChatClients,
    );


    // let client_id = client.id; // ID del client
    // let neighbor_count = client.packet_send.len(); // Numero di vicini

    let mut handles = Vec::new();

    handles.push(thread::spawn(move || {
        client.run();
    }));

    // // Configurazione di eframe
    // let native_options = eframe::NativeOptions::default();
    // let _ = eframe::run_native("My egui App", native_options, Box::new(move |cc| {
    //     Ok(Box::new(MyEguiApp::new(cc, client_id as u32, neighbor_count, client)))
    // }));

    while let Some(handle) = handles.pop() {
        handle
            .join()
            .expect("Error occured while exiting a client");
    }
}