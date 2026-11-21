use std::{
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use eframe::egui;
use vm_update::{Channel, CheckOutcome, GitHubClient, Release, Target, Version, RELEASES_URL};

pub struct Updates {
    pub open: bool,
    channel: Channel,
    automatic: bool,
    due: Instant,
    etag: Option<String>,
    release: Option<Release>,
    last_status: String,
    pending: Option<Receiver<Result<CheckOutcome, String>>>,
}

impl Updates {
    pub fn new(start: Instant, automatic: bool) -> Self {
        Self {
            open: false,
            channel: Channel::Stable,
            automatic,
            due: start + Duration::from_secs(30),
            etag: None,
            release: None,
            last_status: String::new(),
            pending: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(outcome) => {
                    self.pending = None;
                    match outcome {
                        Ok(CheckOutcome::NotModified) => {}
                        Ok(CheckOutcome::Checked { release, etag }) => {
                            self.last_status = if release.is_some() {
                                "发现新版本。".into()
                            } else {
                                "当前渠道没有更新。".into()
                            };
                            self.release = release;
                            self.etag = etag;
                        }
                        Err(message) => self.last_status = message,
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.last_status = "更新检查意外结束。".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        let mut open = self.open;
        let mut manual = false;
        let selected_channel = self.channel;
        egui::Window::new("Velmira 更新")
            .open(&mut open)
            .default_width(390.0)
            .show(ctx, |ui| {
                ui.label(concat!("当前版本 ", env!("CARGO_PKG_VERSION")));
                ui.checkbox(&mut self.automatic, "本次运行自动检查更新");
                ui.add_enabled_ui(self.pending.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.channel, Channel::Stable, "稳定版");
                        ui.selectable_value(&mut self.channel, Channel::Beta, "测试版");
                    });
                    manual = ui.button("检查更新").clicked();
                });
                if self.pending.is_some() {
                    ui.spinner();
                }
                if self.channel == selected_channel {
                    ui.label(&self.last_status);
                    if let Some(release) = &self.release {
                        ui.hyperlink_to(&release.tag, &release.url);
                        if let Some(asset) =
                            Target::current().and_then(|target| release.asset_for(target))
                        {
                            ui.hyperlink_to(&asset.name, &asset.url);
                            if let Some(hash) = asset.sha256() {
                                ui.add(egui::Label::new(format!("SHA-256 {hash}")).wrap());
                            }
                        }
                    }
                }
                ui.hyperlink_to("查看发行页", RELEASES_URL);
            });
        self.open = open;
        if self.channel != selected_channel {
            self.etag = None;
            self.release = None;
            self.last_status.clear();
        }
        if manual || (self.pending.is_none() && self.automatic && Instant::now() >= self.due) {
            self.start(ctx);
        }
        if self.pending.is_none() && self.automatic {
            ctx.request_repaint_after(self.due.saturating_duration_since(Instant::now()));
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.due = Instant::now() + Duration::from_secs(24 * 60 * 60);
        let ctx = ctx.clone();
        let etag = self.etag.clone();
        let channel = self.channel;
        let (send, receive) = bounded(1);
        match thread::Builder::new()
            .name("velmira-update-check".into())
            .spawn(move || {
                let current = Version::parse(env!("CARGO_PKG_VERSION"));
                let outcome = current
                    .map_err(|error| error.to_string())
                    .and_then(|current| {
                        GitHubClient::new()
                            .and_then(|client| client.check(&current, channel, etag.as_deref()))
                            .map_err(|error| error.to_string())
                    });
                let _ = send.send(outcome);
                ctx.request_repaint();
            }) {
            Ok(_) => self.pending = Some(receive),
            Err(error) => self.last_status = error.to_string(),
        }
    }
}
