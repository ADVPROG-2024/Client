use std::collections::HashMap;
use crossbeam_channel::unbounded;
use eframe::egui;
use wg_2024::packet::Packet;
use client::{ClientCommand, ClientEvent, DronegowskiClient};

#[test]
fn test() {
    let (sim_controller_send, _) = unbounded::<ClientEvent>();
    let (_send_controller, controller_receive) = unbounded::<ClientCommand>();
    let (packet_send, packet_receive) = unbounded::<Packet>();

    //Create channel for the neighbor
    let (neighbor_send, neighbor_receive) = unbounded();
    let mut senders = HashMap::new();
    senders.insert(2, neighbor_send); //Drone 2 neighbor

    let client = DronegowskiClient::new(2, sim_controller_send, controller_receive, packet_receive.clone(), senders);
    println!("Hello world!");
    let native_options = eframe::NativeOptions::default();
    eframe::run_native("My egui App", native_options, Box::new(|cc| Ok(Box::new(MyEguiApp::new(cc)))));
}


#[derive(Default)]
struct MyEguiApp {}

impl MyEguiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.
        Self::default()
    }
}

impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello World!");
        });
    }
}