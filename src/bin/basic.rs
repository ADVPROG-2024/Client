use std::collections::HashMap;
use crossbeam_channel::unbounded;
use eframe::egui;
use wg_2024::packet::Packet;
use client::{ClientCommand, ClientEvent, ClientType, DronegowskiClient};

fn main() {
    let (sim_controller_send, _) = unbounded::<ClientEvent>();
    let (_send_controller, controller_receive) = unbounded::<ClientCommand>();
    let (_, packet_receive) = unbounded::<Packet>();

    let (neighbor_send, _) = unbounded();
    let mut senders = HashMap::new();
    senders.insert(2, neighbor_send); // Drone 2 neighbor

    let mut client = DronegowskiClient::new(
        2,
        sim_controller_send,
        controller_receive,
        packet_receive.clone(),
        senders,
    );

    let client_id = client.id; // Prendi l'ID del client
    let neighbor_count = client.packet_send.len(); // Conta il numero di vicini

    let native_options = eframe::NativeOptions::default();
    let _ = eframe::run_native("My egui App", native_options, Box::new(move |cc| {
        Ok(Box::new(MyEguiApp::new(cc, client_id as u32, neighbor_count, client)))
    }));
}


struct MyEguiApp {
    client_id: u32,
    neighbor_count: usize,
    output: String,               // Per mostrare i messaggi
    client_type: ClientType,      // Tipo di client attuale
    client: DronegowskiClient,    // Riferimento al client
}


impl MyEguiApp {
    fn new(cc: &eframe::CreationContext<'_>, client_id: u32, neighbor_count: usize, client: DronegowskiClient) -> Self {
        Self {
            client_id,
            neighbor_count,
            output: String::new(),
            client_type: client.client_type.clone(),
            client,
        }
    }
}


impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Intestazione
            ui.vertical_centered(|ui| {
                ui.heading("🌐 Dronegowski Client Manager");
                ui.label("Gestisci il tuo client in modo intuitivo ed efficiente.");
                ui.add_space(10.0); // Spaziatura
                ui.separator();
            });

            // Informazioni sul client
            ui.group(|ui| {
                ui.heading("🔎 Informazioni del Client");
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
                    if ui.button("🔄 Cambia tipo di client").clicked() {
                        self.client.switch_client_type();
                        self.client_type = self.client.client_type.clone();
                        self.output = format!("Tipo di client cambiato a {:?}", self.client_type);
                    }
                    ui.label("Passa rapidamente da un tipo di client all'altro.");
                });
            });

            ui.add_space(20.0);

            // Funzionalità in base al tipo di client
            ui.group(|ui| {
                ui.heading("⚙️ Funzionalità disponibili");
                ui.separator();
                match self.client_type {
                    ClientType::ChatClients => {
                        ui.label("Funzionalità per Chat Clients:");
                        ui.vertical(|ui| {
                            if ui.button("✅ Registrati al server").clicked() {
                                self.output = match self.client.register_with_server() {
                                    Ok(_) => "Registrazione completata!".to_string(),
                                    Err(err) => format!("Errore: {}", err),
                                };
                            }
                            if ui.button("📋 Ottieni lista utenti").clicked() {
                                self.output = match self.client.request_client_list() {
                                    Ok(users) => format!("Lista utenti: {:?}", users),
                                    Err(err) => format!("Errore: {}", err),
                                };
                            }
                            if ui.button("✉️ Invia messaggio").clicked() {
                                self.output = match self.client.send_message(3, "Ciao!") {
                                    Ok(_) => "Messaggio inviato con successo!".to_string(),
                                    Err(err) => format!("Errore: {}", err),
                                };
                            }
                        });
                    }
                    ClientType::WebBrowsers => {
                        ui.label("Funzionalità per Web Browsers:");
                        ui.vertical(|ui| {
                            if ui.button("📂 Richiedi lista file").clicked() {
                                self.output = match self.client.request_file_list() {
                                    Ok(files) => format!("Lista file: {:?}", files),
                                    Err(err) => format!("Errore: {}", err),
                                };
                            }
                            if ui.button("⬇️ Scarica un file").clicked() {
                                self.output = match self.client.request_file("example.txt") {
                                    Ok(_) => "File scaricato con successo!".to_string(),
                                    Err(err) => format!("Errore: {}", err),
                                };
                            }
                        });
                    }
                }
            });

            ui.add_space(20.0);

            // Pannello dell'output
            ui.group(|ui| {
                ui.heading("📤 Output");
                ui.separator();
                ui.label(&self.output);
            });
        });
    }
}


