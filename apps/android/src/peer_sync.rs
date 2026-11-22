use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use android_activity::AndroidApp;
use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use vm_sync::PairingCode;
use zeroize::Zeroizing;

use crate::{mobile_ui, pairing_cli, saf, theme};

#[derive(Clone, Copy)]
enum Field {
    Address,
    Code,
}

type Reply = Result<(String, Vec<String>, bool), String>;

pub struct PeerSync {
    data: PathBuf,
    address: String,
    code: Zeroizing<String>,
    field: Option<Field>,
    selected: Option<String>,
    peers: Vec<String>,
    status: String,
    task: Option<Receiver<Reply>>,
}

impl PeerSync {
    pub fn new(data: &Path) -> Self {
        Self {
            data: data.to_owned(),
            address: String::new(),
            code: Zeroizing::new(String::new()),
            field: None,
            selected: None,
            peers: Vec::new(),
            status: String::new(),
            task: None,
        }
    }

    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.task.is_none() {
            self.start(ctx, |data| {
                Ok((
                    String::new(),
                    pairing_cli::devices(data).map_err(|e| e.to_string())?,
                    false,
                ))
            });
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, app: &AndroidApp) -> bool {
        if let Some(field) = self.field {
            match saf::take_field(app) {
                Ok(None) => ctx.request_repaint_after(Duration::from_millis(80)),
                Ok(Some(value)) => {
                    self.field = None;
                    if let Some(value) = value {
                        match field {
                            Field::Address => self.address = value.trim().to_owned(),
                            Field::Code => self.code = Zeroizing::new(value),
                        }
                    }
                }
                Err(error) => {
                    self.field = None;
                    self.status = error;
                }
            }
        }
        if let Some(task) = &self.task {
            match task.try_recv() {
                Ok(reply) => {
                    self.task = None;
                    match reply {
                        Ok((status, peers, changed)) => {
                            if !status.is_empty() {
                                self.status = status;
                            }
                            if self.selected.as_ref().is_none_or(|id| !peers.contains(id)) {
                                self.selected = peers.first().cloned();
                            }
                            self.peers = peers;
                            return changed;
                        }
                        Err(error) => self.status = error,
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.task = None;
                    self.status = "同步任务已中断".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        false
    }

    pub fn show(&mut self, ui: &mut egui::Ui, app: &AndroidApp) {
        let p = theme::palette(ui.visuals().dark_mode);
        ui.label("在电脑的 Velmira 原生版启动配对或同步监听，再输入电脑的局域网 IP 与端口。同步进度、书签和批注前，两边需导入相同书籍。");
        ui.add_enabled_ui(self.field.is_none() && self.task.is_none(), |ui| {
            let address = if self.address.is_empty() {
                "主机 IP:端口"
            } else {
                &self.address
            };
            if mobile_ui::list_row(ui, "peer-address", address, p.ink, p.surface2).clicked() {
                self.edit(app, Field::Address);
            }
            let code = if self.code.is_empty() {
                "六位配对码"
            } else {
                "配对码已填写"
            };
            if mobile_ui::list_row(ui, "peer-code", code, p.ink, p.surface2).clicked() {
                self.edit(app, Field::Code);
            }
            if ui
                .add_sized(
                    [122.0, 48.0],
                    egui::Button::new("连接新设备").fill(p.accent_weak),
                )
                .clicked()
            {
                self.connect(ui.ctx());
            }
            if !self.peers.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("已配对设备").strong());
                for id in &self.peers {
                    if mobile_ui::list_row(
                        ui,
                        id,
                        id,
                        p.ink,
                        if self.selected.as_ref() == Some(id) {
                            p.accent_weak
                        } else {
                            p.surface2
                        },
                    )
                    .clicked()
                    {
                        self.selected = Some(id.clone());
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_sized([110.0, 48.0], egui::Button::new("立即同步"))
                        .clicked()
                    {
                        self.sync(ui.ctx());
                    }
                    if ui
                        .add_sized([110.0, 48.0], egui::Button::new("解除配对").frame(false))
                        .clicked()
                    {
                        if let Some(id) = self.selected.clone() {
                            self.start(ui.ctx(), move |data| {
                                pairing_cli::revoke(data, &id).map_err(|e| e.to_string())?;
                                Ok((
                                    "已解除配对".into(),
                                    pairing_cli::devices(data).map_err(|e| e.to_string())?,
                                    false,
                                ))
                            });
                        }
                    }
                });
            }
        });
        if self.task.is_some() {
            ui.spinner();
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }

    fn edit(&mut self, app: &AndroidApp, field: Field) {
        let (title, value, mask, limit) = match field {
            Field::Address => ("主机 IP:端口", self.address.as_str(), 1, 100),
            Field::Code => ("六位配对码", self.code.as_str(), 2, 6),
        };
        match saf::edit_field(app, title, value, mask, limit) {
            Ok(()) => self.field = Some(field),
            Err(error) => self.status = error,
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let Ok(addr) = self.address.parse::<SocketAddr>() else {
            self.status = "请输入有效的 IP:端口".into();
            return;
        };
        let Ok(code) = PairingCode::parse(self.code.trim()) else {
            self.status = "请输入六位配对码".into();
            return;
        };
        self.code = Zeroizing::new(String::new());
        self.start(ctx, move |data| {
            let id = pairing_cli::connect(addr, code, data).map_err(|e| e.to_string())?;
            Ok((
                format!("已配对 {id}"),
                pairing_cli::devices(data).map_err(|e| e.to_string())?,
                false,
            ))
        });
    }

    fn sync(&mut self, ctx: &egui::Context) {
        let Ok(addr) = self.address.parse::<SocketAddr>() else {
            self.status = "请输入同步监听的 IP:端口".into();
            return;
        };
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.start(ctx, move |data| {
            let status = pairing_cli::synchronize(addr, &id, data).map_err(|e| e.to_string())?;
            Ok((
                status,
                pairing_cli::devices(data).map_err(|e| e.to_string())?,
                true,
            ))
        });
    }

    fn start(&mut self, ctx: &egui::Context, work: impl FnOnce(&Path) -> Reply + Send + 'static) {
        let (send, receive) = bounded(1);
        let data = self.data.clone();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("velmira-peer-sync".into())
            .spawn(move || {
                let _ = send.send(work(&data));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.status.clear();
                self.task = Some(receive);
            }
            Err(e) => self.status = e.to_string(),
        }
    }
}
