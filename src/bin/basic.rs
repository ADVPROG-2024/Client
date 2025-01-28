use std::collections::HashMap;
use std::fs::File;
use std::thread;
use std::time::Duration;
use crossbeam_channel::unbounded;
use eframe::egui;
use log::LevelFilter;
use simplelog::{ConfigBuilder, WriteLogger};
use wg_2024::packet::{Fragment, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use client::{ClientType, DronegowskiClient};
use dronegowski_utils::hosts::{ClientCommand, ClientEvent, ClientType, TestMessage};
use dronegowski_utils::functions::simple_log;
use wg_2024::packet::PacketType::MsgFragment;

fn main() {

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

struct MyEguiApp {
    client_id: u32,
    neighbor_count: usize,
    output: String,               // Per mostrare i messaggi
    client_type: ClientType,      // Tipo di client attuale
    client: DronegowskiClient,    // Riferimento al client
    received_messages: Vec<String>, // Memorizza i messaggi ricevuti
}

impl MyEguiApp {
    fn new(cc: &eframe::CreationContext<'_>, client_id: u32, neighbor_count: usize, client: DronegowskiClient) -> Self {
        Self {
            client_id,
            neighbor_count,
            output: String::new(),
            client_type: client.client_type.clone(),
            client,
            received_messages: Vec::new(),
        }
    }
}

impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Intestazione
            ui.vertical_centered(|ui| {
                ui.heading("Dronegowski Client Manager");
                ui.label("Gestisci il tuo client in modo intuitivo ed efficiente.");
                ui.add_space(10.0);
                ui.separator();
            });

            // Informazioni sul client
            ui.group(|ui| {
                ui.heading("Informazioni del Client");
                ui.horizontal(|ui| {
                    ui.label("ID del client:");
                    ui.monospace(self.client_id.to_string());
                });
                ui.horizontal(|ui| {
                    ui.label("Numero di vicini:");
                    ui.monospace(self.neighbor_count.to_string());
                });
                ui.horizontal(|ui| {
                    ui.label("Tipo di client attuale:");
                    ui.monospace(format!("{:?}", self.client_type));
                });
            });

            ui.add_space(10.0);

            // Bottone per cambiare il tipo di client
            ui.group(|ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Cambia tipo di client").clicked() {
                        self.client.switch_client_type();
                        self.client_type = self.client.client_type.clone();
                        self.output = format!("Tipo di client cambiato a {:?}", self.client_type);
                    }
                    ui.label("Passa rapidamente da un tipo di client all'altro.");
                });
            });

            ui.add_space(20.0);

            // Mostra i messaggi ricevuti
            ui.group(|ui| {
                ui.heading("Messaggi ricevuti");
                ui.separator();
                for msg in &self.received_messages {
                    ui.label(msg);
                }
            });

            ui.add_space(20.0);

            // Pannello dell'output
            ui.group(|ui| {
                ui.heading("Output");
                ui.separator();
                ui.label(&self.output);
            });
        });
    }
}
