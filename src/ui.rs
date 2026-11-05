use crate::{
    collectors::Collector,
    model::*,
    proxy::ProxyHandle,
    storage::{Store, export_csv},
};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDateTime, TimeZone, Utc};
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2, vec2};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BG: Color32 = Color32::from_rgb(13, 16, 22);
const PANEL: Color32 = Color32::from_rgb(20, 24, 32);
const BORDER: Color32 = Color32::from_rgb(40, 47, 60);
const TEXT: Color32 = Color32::from_rgb(230, 235, 244);
const MUTED: Color32 = Color32::from_rgb(139, 151, 171);
const ACCENT: Color32 = Color32::from_rgb(100, 222, 186);
const AMBER: Color32 = Color32::from_rgb(244, 189, 104);
const RED: Color32 = Color32::from_rgb(247, 126, 132);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    History,
    Connections,
    Settings,
}
enum WorkerMessage {
    Started,
    Finished(Snapshot),
}
struct Refresh {
    config: Config,
    force: bool,
}
#[derive(Default)]
struct ManualEntry {
    provider: usize,
    model: String,
    date: String,
    input: u64,
    output: u64,
    cached: u64,
    has_cost: bool,
    cost: f64,
    has_credits: bool,
    credits: f64,
}

pub struct TokenScope {
    store: Store,
    config: Config,
    snapshot: Snapshot,
    page: Page,
    period: ViewPeriod,
    filter: Option<String>,
    search: String,
    chart_scale: usize,
    chart_metric: usize,
    history_page: usize,
    editor: usize,
    dirty: bool,
    busy: bool,
    sender: mpsc::Sender<Refresh>,
    receiver: mpsc::Receiver<WorkerMessage>,
    warning: Option<String>,
    toast: Option<(String, bool, Instant)>,
    new_provider: Option<ProviderConfig>,
    manual: Option<ManualEntry>,
    reset_inputs: HashMap<String, String>,
    proxy: Option<ProxyHandle>,
    proxy_signature: Option<(u16, String, String)>,
    capture: Option<std::path::PathBuf>,
    capture_requested: bool,
    launched: Instant,
    key_input: String,
    key_for: Option<String>,
}

impl TokenScope {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        store: Store,
        config: Config,
        warning: Option<String>,
    ) -> Self {
        style(&cc.egui_ctx);
        let (sender, requests) = mpsc::channel::<Refresh>();
        let (responses, receiver) = mpsc::channel();
        let worker_store = store.clone();
        let worker_ctx = cc.egui_ctx.clone();
        let initial = config.clone();
        thread::Builder::new()
            .name("usage-collector".into())
            .spawn(move || {
                let mut collector = Collector::new(worker_store);
                let mut settings = initial;
                loop {
                    let force = match requests.recv_timeout(Duration::from_secs(
                        settings.refresh_seconds.clamp(15, 3600),
                    )) {
                        Ok(request) => {
                            settings = request.config;
                            request.force
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => false,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if responses.send(WorkerMessage::Started).is_err() {
                        break;
                    }
                    worker_ctx.request_repaint();
                    let result = collector.refresh(&settings, force);
                    if responses.send(WorkerMessage::Finished(result)).is_err() {
                        break;
                    }
                    worker_ctx.request_repaint();
                }
            })
            .expect("Impossible de démarrer la collecte");
        let _ = sender.send(Refresh {
            config: config.clone(),
            force: true,
        });
        let arguments: Vec<String> = std::env::args().collect();
        let capture = arguments
            .windows(2)
            .find(|a| a[0] == "--screenshot")
            .map(|a| std::path::PathBuf::from(&a[1]));
        // Optional zoom applies only to automated screenshots, never normal use.
        if capture.is_some()
            && let Some(zoom) = arguments.windows(2).find(|a| a[0] == "--capture-zoom").and_then(|a| a[1].parse::<f32>().ok()).filter(|v| (0.5..=1.0).contains(v))
        {
            cc.egui_ctx.set_zoom_factor(zoom);
        }
        let start_page = arguments
            .windows(2)
            .find(|a| a[0] == "--page")
            .map(|a| a[1].as_str());
        let initial_chart_metric = usize::from(
            config.providers.iter().any(|p| p.enabled)
                && config
                    .providers
                    .iter()
                    .filter(|p| p.enabled)
                    .all(|p| p.kind == ProviderKind::OllamaCloud),
        );
        let initial_filter = arguments
            .windows(2)
            .find(|a| a[0] == "--provider")
            .and_then(|a| config.providers.iter().find(|p| p.id == a[1] && p.enabled))
            .map(|p| p.id.clone());
        let initial_editor = initial_filter
            .as_ref()
            .and_then(|id| config.providers.iter().position(|p| &p.id == id))
            .unwrap_or(0);
        let mut app = Self {
            store,
            config,
            snapshot: Snapshot::default(),
            page: match start_page {
                Some("history") => Page::History,
                Some("connections") => Page::Connections,
                Some("settings") => Page::Settings,
                _ => Page::Overview,
            },
            period: ViewPeriod::Week,
            filter: initial_filter,
            search: String::new(),
            chart_scale: 0,
            chart_metric: initial_chart_metric,
            history_page: 0,
            editor: initial_editor,
            dirty: false,
            busy: true,
            sender,
            receiver,
            warning,
            toast: None,
            new_provider: None,
            manual: None,
            reset_inputs: HashMap::new(),
            proxy: None,
            proxy_signature: None,
            capture,
            capture_requested: false,
            launched: Instant::now(),
            key_input: String::new(),
            key_for: None,
        };
        app.update_proxy();
        app
    }
    pub fn icon() -> egui::IconData {
        let size = 64;
        let mut rgba = vec![0; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let c = if (12..22).contains(&x) && (33..50).contains(&y)
                    || (27..37).contains(&x) && (22..50).contains(&y)
                    || (42..52).contains(&x) && (12..50).contains(&y)
                {
                    ACCENT
                } else {
                    PANEL
                };
                let i = (y * size + x) * 4;
                rgba[i..i + 4].copy_from_slice(&c.to_array());
            }
        }
        egui::IconData {
            rgba,
            width: size as u32,
            height: size as u32,
        }
    }
    fn refresh(&mut self) {
        self.busy = true;
        if self
            .sender
            .send(Refresh {
                config: self.config.clone(),
                force: true,
            })
            .is_err()
        {
            self.busy = false;
            self.notify(
                "Le collecteur s’est arrêté. Relancez l’application.".into(),
                true,
            );
        }
    }
    fn notify(&mut self, message: String, error: bool) {
        self.toast = Some((message, error, Instant::now()));
    }
    fn save(&mut self) {
        for provider in &self.config.providers {
            for (index, budget) in provider.limits.iter().enumerate() {
                for (active, key) in [
                    (
                        budget.manual_used.is_some(),
                        format!("{}:{index}", provider.id),
                    ),
                    (
                        budget.period == BudgetPeriod::FiveHours,
                        format!("anchor:{}:{index}", provider.id),
                    ),
                ] {
                    if active
                        && self
                            .reset_inputs
                            .get(&key)
                            .is_some_and(|text| parse_local(text).is_err())
                    {
                        self.notify(
                            "Une date de limite est invalide. Format attendu : AAAA-MM-JJ HH:MM."
                                .into(),
                            true,
                        );
                        return;
                    }
                }
            }
        }
        match self.store.save_config(&self.config) {
            Ok(()) => {
                self.dirty = false;
                self.warning = None;
                self.update_proxy();
                self.refresh();
                self.notify("Configuration enregistrée.".into(), false);
            }
            Err(e) => self.notify(e, true),
        }
    }
    fn update_proxy(&mut self) {
        let provider = self
            .config
            .providers
            .iter()
            .find(|p| p.enabled && p.kind == ProviderKind::Ollama)
            .map(|p| p.id.clone());
        let signature = provider
            .map(|id| (self.config.proxy_port, self.config.ollama_url.clone(), id))
            .filter(|_| self.config.proxy_enabled);
        if self.proxy_signature == signature {
            return;
        }
        self.proxy = None;
        self.proxy_signature = None;
        if let Some((port, url, id)) = signature {
            match ProxyHandle::start(port, &url, self.store.clone(), id.clone()) {
                Ok(handle) => {
                    self.proxy = Some(handle);
                    self.proxy_signature = Some((port, url, id));
                }
                Err(e) => self.notify(format!("Collecteur Ollama : {e}"), true),
            }
        }
    }
    fn selected_events(&self) -> Vec<&UsageEvent> {
        let now = Utc::now();
        let search = self.search.to_lowercase();
        self.snapshot
            .events
            .iter()
            .filter(|e| {
                self.period.contains(e.timestamp, now)
                    && self.filter.as_ref().is_none_or(|id| id == &e.provider)
                    && (self.page != Page::History
                        || search.is_empty()
                        || e.model.to_lowercase().contains(&search))
            })
            .collect()
    }
    fn export(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!(
                "tokenscope-{}.csv",
                Local::now().format("%Y-%m-%d")
            ))
            .add_filter("CSV", &["csv"])
            .save_file()
        {
            let result = export_csv(&path, &self.selected_events());
            match result {
                Ok(()) => self.notify(format!("Export enregistré : {}", path.display()), false),
                Err(e) => self.notify(e, true),
            }
        }
    }
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(210.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(16, 20, 27))
                    .inner_margin(18),
            )
            .show(ctx, |ui| {
                ui.add_space(11.0);
                ui.horizontal(|ui| {
                    logo(ui);
                    ui.label(RichText::new("TokenScope").size(22.0).strong());
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new("VOTRE CONSOMMATION IA")
                        .size(9.5)
                        .color(MUTED),
                );
                let menu_height = (ui.available_height() - 145.0).max(100.0);
                egui::ScrollArea::vertical()
                    .id_salt("sidebar-menu")
                    .max_height(menu_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(37.0);
                        for (page, icon, text) in [
                            (Page::Overview, ">", "Vue d’ensemble"),
                            (Page::History, "≡", "Historique"),
                            (Page::Connections, "⊞", "Connexions"),
                            (Page::Settings, "⚙", "Réglages"),
                        ] {
                            let active = self.page == page;
                            let button = egui::Button::new(
                                RichText::new(format!("{icon}   {text}"))
                                    .size(14.0)
                                    .color(if active { ACCENT } else { MUTED }),
                            )
                            .fill(if active {
                                Color32::from_rgb(29, 49, 47)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(8.0)
                            .min_size(vec2(ui.available_width(), 41.0));
                            if ui.add(button).clicked() {
                                self.page = page;
                                self.history_page = 0;
                            }
                            ui.add_space(5.0);
                        }
                        ui.add_space(28.0);
                        ui.label(RichText::new("FOURNISSEURS").size(10.0).color(MUTED));
                        ui.add_space(11.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Tous les comptes")
                                        .color(if self.filter.is_none() { TEXT } else { MUTED }),
                                )
                                .fill(Color32::TRANSPARENT)
                                .min_size(vec2(ui.available_width(), 30.0)),
                            )
                            .clicked()
                        {
                            self.filter = None;
                            self.history_page = 0;
                        }
                        let providers = self.config.providers.clone();
                        for p in providers.iter().filter(|p| p.enabled) {
                            ui.horizontal(|ui| {
                                dot(ui, Color32::from_rgb(p.color[0], p.color[1], p.color[2]));
                                let selected = self.filter.as_deref() == Some(&p.id);
                                if ui
                                    .selectable_label(
                                        selected,
                                        RichText::new(&p.name).size(13.0).color(if selected {
                                            TEXT
                                        } else {
                                            MUTED
                                        }),
                                    )
                                    .clicked()
                                {
                                    self.filter = Some(p.id.clone());
                                    if p.kind == ProviderKind::OllamaCloud {
                                        self.chart_metric = 1;
                                    }
                                    self.history_page = 0;
                                    if self.page == Page::Connections {
                                        self.editor = self
                                            .config
                                            .providers
                                            .iter()
                                            .position(|a| a.id == p.id)
                                            .unwrap_or(0);
                                    }
                                }
                            });
                            ui.add_space(6.0);
                        }
                        ui.add_space(12.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+  Ajouter un fournisseur")
                                        .size(11.5)
                                        .color(MUTED),
                                )
                                .fill(Color32::TRANSPARENT),
                            )
                            .clicked()
                        {
                            self.new_provider = Some(ProviderConfig::default());
                        }
                    });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("TokenScope  {}", env!("CARGO_PKG_VERSION")))
                            .size(10.5)
                            .color(MUTED),
                    );
                    ui.label(
                        RichText::new(format!("Heure locale · {}", Local::now().format("UTC%:z")))
                            .size(10.0)
                            .color(MUTED),
                    );
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        dot(ui, if self.busy { AMBER } else { ACCENT });
                        ui.label(
                            RichText::new(if self.busy {
                                "Synchronisation…"
                            } else {
                                "Suivi automatique"
                            })
                            .size(11.0),
                        );
                    });
                    if let Some(time) = self.snapshot.refreshed_at {
                        ui.label(
                            RichText::new(format!(
                                "Dernier scan à {}",
                                time.with_timezone(&Local).format("%H:%M:%S")
                            ))
                            .size(10.0)
                            .color(MUTED),
                        );
                    }
                });
            });
    }
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("ESPACE PERSONNEL  /  CONSOMMATION")
                    .size(10.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(Local::now().format("%d/%m/%Y  ·  %H:%M:%S").to_string())
                        .size(11.0)
                        .color(MUTED),
                );
            });
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(match self.page {
                        Page::Overview => "Vos limites, en un regard.",
                        Page::History => "Chaque token compte.",
                        Page::Connections => "Vos comptes, connectés.",
                        Page::Settings => "Un suivi à votre mesure.",
                    })
                    .size(27.0)
                    .strong(),
                );
                ui.add_space(5.0);
                ui.label(
                    RichText::new(match self.page {
                        Page::Overview => "Consommation, crédits et prochaines réinitialisations.",
                        Page::History => "Retrouvez votre consommation par jour, semaine et mois.",
                        Page::Connections => {
                            "Connectez vos sources et définissez vos propres limites."
                        }
                        Page::Settings => "Actualisation, alertes et collecte locale Ollama.",
                    })
                    .size(12.5)
                    .color(MUTED),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Button::new("↻  Actualiser").min_size(vec2(115.0, 34.0)),
                    )
                    .clicked()
                {
                    self.refresh();
                }
                if self.busy {
                    ui.spinner();
                }
            });
        });
        ui.add_space(24.0);
        if self.dirty {
            egui::Frame::new()
                .fill(Color32::from_rgb(48, 40, 27))
                .corner_radius(8.0)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Modifications en attente").color(AMBER));
                        if ui.button("Enregistrer").clicked() {
                            self.save();
                        }
                    });
                });
            ui.add_space(12.0);
        }
        if self.warning.is_some() || !self.snapshot.warnings.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(format!(
                    "Diagnostic · {} point(s) à vérifier",
                    self.snapshot.warnings.len() + usize::from(self.warning.is_some())
                ))
                .size(11.0)
                .color(AMBER),
            )
            .show(ui, |ui| {
                if let Some(warning) = &self.warning {
                    ui.label(warning);
                }
                for warning in &self.snapshot.warnings {
                    ui.label(RichText::new(warning).size(11.0));
                }
            });
            ui.add_space(8.0);
        }
    }
    fn period_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(PANEL)
                .corner_radius(7.0)
                .inner_margin(4)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        for period in ViewPeriod::ALL {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(period.label()).size(11.5).color(
                                            if self.period == period { TEXT } else { MUTED },
                                        ),
                                    )
                                    .fill(if self.period == period {
                                        Color32::from_rgb(48, 57, 71)
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .min_size(vec2(90.0, 27.0))
                                    .corner_radius(5.0),
                                )
                                .clicked()
                            {
                                self.period = period;
                                self.history_page = 0;
                            }
                        }
                    });
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↓  Export CSV").clicked() {
                    self.export();
                }
                if let Some(id) = &self.filter
                    && let Some(p) = self.config.providers.iter().find(|p| &p.id == id)
                {
                    pill(
                        ui,
                        &p.name,
                        Color32::from_rgb(p.color[0], p.color[1], p.color[2]),
                    );
                }
            });
        });
        ui.add_space(18.0);
    }
    fn overview(&mut self, ui: &mut egui::Ui) {
        self.period_selector(ui);
        let now = Utc::now();
        let events = self.selected_events();
        let total = events.iter().map(|e| e.total()).sum::<u64>();
        let cached = events.iter().map(|e| e.cached_tokens).sum::<u64>();
        let input = events.iter().map(|e| e.input_tokens).sum::<u64>();
        let unknown_token_count = self.filter.as_ref().is_some_and(|id| {
            self.config
                .providers
                .iter()
                .any(|p| &p.id == id && p.kind == ProviderKind::Antigravity)
        }) && !events.iter().any(|e| e.tokens_reported);
        let mut seen = HashSet::new();
        let mut quotas = vec![];
        for report in &self.snapshot.providers {
            if self.filter.as_ref().is_some_and(|id| id != &report.id) {
                continue;
            }
            let key = report.shared_group.as_ref().unwrap_or(&report.id);
            if seen.insert(key.clone()) {
                quotas.extend(report.quotas.iter());
            }
        }
        let snapshot_events = &self.snapshot.events;
        let personal: Vec<_> = self
            .config
            .providers
            .iter()
            .filter(|p| p.enabled && self.filter.as_ref().is_none_or(|id| id == &p.id))
            .flat_map(|p| {
                p.limits
                    .iter()
                    .map(move |limit| limit.quota(&p.id, snapshot_events, now))
            })
            .collect();
        let attention = quotas
            .iter()
            .copied()
            .chain(personal.iter())
            .filter(|q| q.used_percent >= self.config.alert_percent)
            .count();
        let next = quotas
            .iter()
            .copied()
            .chain(personal.iter())
            .filter_map(|q| q.resets_at)
            .filter(|t| *t > now)
            .min();
        let cost_count = events.iter().filter(|e| e.cost_usd.is_some()).count();
        let cost = events.iter().filter_map(|e| e.cost_usd).sum::<f64>();
        ui.columns(4,|cols| {
            metric(&mut cols[0],"TOKENS CONNUS",&if unknown_token_count{"—".into()}else{compact(total as f64)},&if unknown_token_count{"Compteur non communiqué par Google".into()}else{format!("{} relevés · {}",events.len(),self.period.label().to_lowercase())},ACCENT)
                .on_hover_text(if unknown_token_count{"Google fournit des fractions de quota, sans historique détaillé de tokens sur cette source.".into()}else{format!("{total} tokens au total, dont {cached} tokens en cache. Les tokens de cache sont déjà inclus dans les entrées.")});
            metric(&mut cols[1],"PROCHAINE RÉINITIALISATION",&next.map(|t|short_countdown(t,now)).unwrap_or_else(||"—".into()),&next.map(|t|t.with_timezone(&Local).format("Le %d/%m à %H:%M").to_string()).unwrap_or_else(||"Aucune échéance connue".into()),TEXT);
            metric(&mut cols[2],"QUOTAS À SURVEILLER",&attention.to_string(),&format!("Seuil d’alerte à {:.0} %",self.config.alert_percent),if attention>0{AMBER}else{ACCENT});
            metric(&mut cols[3],"COÛT DÉCLARÉ",&if cost_count>0{format!("{cost:.2} $")}else{"—".into()},&if cost_count>0{format!("{cost_count}/{} relevés avec coût",events.len())}else{"Coût non fourni par les journaux".into()},TEXT);
        });
        ui.add_space(25.0);
        section(
            ui,
            "Limites de vos comptes",
            "Les jauges suivent les fenêtres du fournisseur.",
        );
        ui.add_space(13.0);
        let providers: Vec<_> = self
            .config
            .providers
            .iter()
            .filter(|p| p.enabled && self.filter.as_ref().is_none_or(|id| id == &p.id))
            .cloned()
            .collect();
        let mut displayed = HashSet::new();
        let mut cards = vec![];
        for p in providers {
            let report = self
                .snapshot
                .providers
                .iter()
                .find(|r| r.id == p.id)
                .cloned()
                .unwrap_or_else(|| ProviderSnapshot {
                    id: p.id.clone(),
                    ..Default::default()
                });
            let key = report.shared_group.clone().unwrap_or_else(|| p.id.clone());
            if !displayed.insert(key) {
                continue;
            }
            let shared: Vec<_> = if self.filter.is_none() {
                self.snapshot
                    .providers
                    .iter()
                    .filter(|r| r.shared_group.is_some() && r.shared_group == report.shared_group)
                    .map(|r| r.id.clone())
                    .collect()
            } else {
                vec![]
            };
            cards.push((p, report, shared));
        }
        let width = ui.available_width();
        let columns = if width > 1000.0 {
            3
        } else if width > 650.0 {
            2
        } else {
            1
        };
        for row in cards.chunks(columns) {
            ui.columns(row.len(), |cols| {
                for (i, (p, r, shared)) in row.iter().enumerate() {
                    self.provider_card(&mut cols[i], p, r, shared, now);
                }
            });
            ui.add_space(12.0);
        }
        if cards.is_empty() {
            ui.label("Aucun fournisseur activé. Ajoutez un compte depuis Connexions.");
        }
        ui.add_space(12.0);
        self.activity_chart(ui);
        ui.add_space(16.0);
        let ratio = if input > 0 {
            cached as f64 / input as f64 * 100.0
        } else {
            0.0
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("Cache : {ratio:.1} % des tokens d’entrée"))
                    .size(11.0)
                    .color(MUTED),
            );
            ui.label(
                RichText::new(
                    " · Les tokens locaux ne se convertissent pas en pourcentage d’abonnement.",
                )
                .size(10.5)
                .color(MUTED),
            );
        });
    }
    fn provider_card(
        &mut self,
        ui: &mut egui::Ui,
        p: &ProviderConfig,
        r: &ProviderSnapshot,
        shared: &[String],
        now: DateTime<Utc>,
    ) {
        let color = Color32::from_rgb(p.color[0], p.color[1], p.color[2]);
        card().show(ui, |ui| {
            ui.set_min_height(210.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.horizontal(|ui| {
                provider_icon(ui, p.kind, color);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(if shared.len() > 1 {
                            "Claude & Claude Code"
                        } else {
                            &p.name
                        })
                        .size(17.0)
                        .strong(),
                    );
                    ui.label(
                        RichText::new(r.plan.as_deref().unwrap_or(match p.kind {
                            ProviderKind::OllamaCloud => "Compte Cloud · clé API",
                            ProviderKind::Ollama => "Modèles locaux",
                            ProviderKind::Antigravity => "Abonnement Google AI",
                            ProviderKind::Claude | ProviderKind::ClaudeCode => "Abonnement commun",
                            _ => "Suivi de consommation",
                        }))
                        .size(10.5)
                        .color(MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let fresh =
                        r.live && r.observed_at.is_some_and(|t| (now - t).num_minutes() < 10);
                    pill(
                        ui,
                        if fresh {
                            "À JOUR"
                        } else if !r.quotas.is_empty() {
                            "RELEVÉ DATÉ"
                        } else {
                            "À CONNECTER"
                        },
                        if fresh { ACCENT } else { MUTED },
                    );
                });
            });
            ui.add_space(17.0);
            let quotas = &r.quotas;
            let visible_quota_count = if p.kind == ProviderKind::Antigravity { 5 } else { 2 };
            for quota in quotas.iter().take(visible_quota_count) {
                quota_row(
                    ui,
                    quota,
                    color,
                    now,
                    self.config.alert_percent,
                    false,
                    r.observed_at,
                );
            }
            if quotas.len() > visible_quota_count {
                egui::CollapsingHeader::new(
                    RichText::new(format!("{} autre(s) limite(s)", quotas.len() - visible_quota_count))
                        .size(10.5)
                        .color(MUTED),
                )
                .id_salt(format!("more-{}", p.id))
                .default_open(
                    quotas
                        .iter()
                        .skip(visible_quota_count)
                        .any(|q| q.used_percent >= self.config.alert_percent),
                )
                .show(ui, |ui| {
                    for quota in quotas.iter().skip(visible_quota_count) {
                        quota_row(
                            ui,
                            quota,
                            color,
                            now,
                            self.config.alert_percent,
                            false,
                            r.observed_at,
                        );
                    }
                });
            }
            for owner in self
                .config
                .providers
                .iter()
                .filter(|a| a.id == p.id || shared.contains(&a.id))
            {
                for budget in &owner.limits {
                    let mut quota = budget.quota(&owner.id, &self.snapshot.events, now);
                    if shared.len() > 1 {
                        quota.name = format!("{} · {}", owner.name, quota.name);
                    }
                    quota_row(
                        ui,
                        &quota,
                        color,
                        now,
                        self.config.alert_percent,
                        true,
                        Some(now),
                    );
                }
            }
            if quotas.is_empty() && p.limits.is_empty() {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(match p.kind {
                        ProviderKind::Ollama => "Pas de quota fournisseur en local",
                        _ => "Limite non disponible",
                    })
                    .size(14.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if p.kind == ProviderKind::Ollama {
                        "Définissez un budget pour suivre votre objectif."
                    } else if p.kind == ProviderKind::Jsonl {
                        "Importez vos relevés et ajoutez une limite."
                    } else {
                        &r.status
                    })
                    .size(11.0)
                    .color(MUTED),
                );
                ui.add_space(12.0);
                if ui
                    .small_button(if p.kind == ProviderKind::Ollama {
                        "Configurer la collecte"
                    } else {
                        "Configurer ce compte"
                    })
                    .clicked()
                {
                    self.editor = self
                        .config
                        .providers
                        .iter()
                        .position(|a| a.id == p.id)
                        .unwrap_or(0);
                    self.page = Page::Connections;
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(9.0);
            let events: Vec<_> = self
                .snapshot
                .events
                .iter()
                .filter(|e| {
                    (e.provider == p.id || shared.contains(&e.provider))
                        && self.period.contains(e.timestamp, now)
                })
                .collect();
            let total = events.iter().map(|e| e.total()).sum::<u64>();
            let requests = events.iter().map(|e| e.request_count).sum::<u64>();
            let known = events.iter().any(|e| e.tokens_reported);
            ui.label(
                RichText::new(if p.kind == ProviderKind::Antigravity && !known {
                    "Tokens non communiqués par Google".into()
                } else if p.kind == ProviderKind::Qoder && !known {
                    "Tokens non communiqués par Qoder".into()
                } else if p.kind == ProviderKind::OllamaCloud && !known {
                    format!(
                        "{requests} requêtes · {}",
                        self.period.label().to_lowercase()
                    )
                } else {
                    format!(
                        "{} tokens · {}",
                        compact(total as f64),
                        self.period.label().to_lowercase()
                    )
                })
                .size(11.0)
                .color(TEXT),
            )
            .on_hover_text(format!(
                "{total} tokens connus · {requests} requêtes enregistrées"
            ));
            if let Some(balance) = &r.credit_balance {
                ui.label(
                    RichText::new(if r.unlimited_credits {
                        "Crédits illimités".into()
                    } else if p.kind == ProviderKind::OllamaCloud {
                        format!("Crédits achetés : {balance}")
                    } else if p.kind == ProviderKind::Antigravity {
                        format!("Solde disponible : {balance}")
                    } else {
                        format!("Crédits additionnels : {balance}")
                    })
                    .size(10.0)
                    .color(MUTED),
                )
                .on_hover_text("Ce solde est distinct du quota inclus dans l’abonnement.");
            } else if p.kind == ProviderKind::Antigravity {
                ui.label(
                    RichText::new("Crédits : montant non communiqué")
                        .size(10.0)
                        .color(MUTED),
                );
            }
            if let Some(note) = &r.usage_note {
                ui.label(RichText::new(note).size(9.5).color(MUTED));
            }
            if let Some(time) = r.observed_at {
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        r.source,
                        time.with_timezone(&Local).format("%d/%m %H:%M:%S")
                    ))
                    .size(9.5)
                    .color(MUTED),
                );
            }
            if !r.live && !r.status.is_empty() {
                ui.label(
                    RichText::new(&r.status)
                        .size(10.0)
                        .color(if r.observed_at.is_some() {
                            AMBER
                        } else {
                            MUTED
                        }),
                );
            }
            if shared.len() > 1 {
                ui.label(
                    RichText::new("Quota partagé · compté une seule fois")
                        .size(9.5)
                        .color(MUTED),
                );
            }
        });
    }
    fn activity_chart(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Votre rythme de consommation")
                            .size(16.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("Historique glissant · données enregistrées")
                            .size(10.0)
                            .color(MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (index, label) in [(2, "12 mois"), (1, "12 semaines"), (0, "7 jours")] {
                        if ui
                            .selectable_label(
                                self.chart_scale == index,
                                RichText::new(label).size(10.5),
                            )
                            .clicked()
                        {
                            self.chart_scale = index;
                        }
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                for (index,label) in ["Tokens connus","Requêtes","Valeur USD"].into_iter().enumerate() {
                    ui.selectable_value(&mut self.chart_metric,index,RichText::new(label).size(10.5));
                }
            });
            ui.add_space(12.0);
            let date = Local::now().date_naive();
            let count = if self.chart_scale == 0 { 7 } else { 12 };
            let dates: Vec<_> = (0..count)
                .rev()
                .map(|offset| match self.chart_scale {
                    1 => {
                        date - ChronoDuration::days(
                            date.weekday().num_days_from_monday() as i64 + offset as i64 * 7,
                        )
                    }
                    2 => {
                        let absolute = date.year() * 12 + date.month0() as i32 - offset;
                        chrono::NaiveDate::from_ymd_opt(
                            absolute.div_euclid(12),
                            absolute.rem_euclid(12) as u32 + 1,
                            1,
                        )
                        .unwrap()
                    }
                    _ => date - ChronoDuration::days(offset as i64),
                })
                .collect();
            let enabled: Vec<_> = self
                .config
                .providers
                .iter()
                .filter(|p| p.enabled && self.filter.as_ref().is_none_or(|id| id == &p.id))
                .collect();
            let mut values = vec![vec![0.0_f64; enabled.len()]; count as usize];
            let now = Utc::now();
            for e in &self.snapshot.events {
                if e.timestamp > now {
                    continue;
                }
                let d = e.timestamp.with_timezone(&Local).date_naive();
                if let Some(bin) = dates.iter().rposition(|start| d >= *start)
                    && let Some(index) = enabled.iter().position(|p| p.id == e.provider)
                {
                    values[bin][index] += match self.chart_metric {1=>e.request_count as f64,2=>e.cost_usd.unwrap_or(0.0),_=>if e.tokens_reported {e.total()as f64}else{0.0}};
                }
            }
            let maximum = values
                .iter()
                .map(|v| v.iter().sum::<f64>())
                .fold(0.0_f64,f64::max)
                .max(if self.chart_metric==2 {0.01}else{1.0}) as f32;
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 184.0), egui::Sense::hover());
            let plot =
                egui::Rect::from_min_max(rect.min + vec2(50.0, 4.0), rect.max - vec2(8.0, 27.0));
            for i in 0..=3 {
                let y = plot.bottom() - plot.height() * i as f32 / 3.0;
                ui.painter().line_segment(
                    [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                    Stroke::new(1.0_f32, Color32::from_rgb(34, 41, 53)),
                );
                ui.painter().text(
                    egui::pos2(plot.left() - 8.0, y),
                    egui::Align2::RIGHT_CENTER,
                    if self.chart_metric==2 {format!("{:.2} $",maximum*i as f32/3.0)}else{compact((maximum * i as f32 / 3.0) as f64)},
                    FontId::proportional(9.5),
                    MUTED,
                );
            }
            let step = plot.width() / count as f32;
            for (index, parts) in values.iter().enumerate() {
                let x = plot.left() + step * (index as f32 + 0.5);
                let mut y = plot.bottom();
                let bar_width = (step * 0.5).clamp(8.0, 42.0);
                for (provider, value) in enabled.iter().zip(parts) {
                    let height = (*value as f32 / maximum * plot.height()).max(if *value > 0.0 {
                        1.0
                    } else {
                        0.0
                    });
                    if height > 0.0 {
                        ui.painter().rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(x - bar_width / 2.0, y - height),
                                egui::pos2(x + bar_width / 2.0, y),
                            ),
                            3.0,
                            Color32::from_rgb(
                                provider.color[0],
                                provider.color[1],
                                provider.color[2],
                            ),
                        );
                    }
                    y -= height;
                }
                ui.painter().text(
                    egui::pos2(x, plot.bottom() + 15.0),
                    egui::Align2::CENTER_CENTER,
                    dates[index]
                        .format(if self.chart_scale == 2 {
                            "%m/%y"
                        } else {
                            "%d/%m"
                        })
                        .to_string(),
                    FontId::proportional(9.0),
                    MUTED,
                );
                if response.hover_pos().is_some_and(|pos| {
                    pos.x > plot.left() + step * index as f32
                        && pos.x < plot.left() + step * (index + 1) as f32
                }) {
                    ui.painter().vline(
                        x,
                        plot.y_range(),
                        Stroke::new(1.0_f32, MUTED.gamma_multiply(0.4)),
                    );
                    response.clone().on_hover_ui(|ui| {
                        ui.label(dates[index].format("%d/%m/%Y").to_string());
                        for (p, v) in enabled.iter().zip(parts) {
                            ui.label(format!("{} : {v:.2} {}", p.name,match self.chart_metric {1=>"requêtes",2=>"USD",_=>"tokens connus"}));
                        }
                    });
                }
            }
            if values.iter().flatten().all(|v| *v == 0.0) {
                ui.painter().text(
                    plot.center(),
                    egui::Align2::CENTER_CENTER,
                    if self.chart_metric==0 && self.snapshot.events.iter().any(|e|!e.tokens_reported&&enabled.iter().any(|p|p.id==e.provider)) {"Compteur de tokens non communiqué · voir Requêtes"}else{"Aucune consommation enregistrée sur cette période"},
                    FontId::proportional(12.0),
                    MUTED,
                );
            }
            ui.add_space(9.0);
            if self.chart_metric==0 && self.snapshot.events.iter().any(|e|!e.tokens_reported&&enabled.iter().any(|p|p.id==e.provider)) {
                ui.label(RichText::new("Ollama Cloud ne communique pas tous les tokens : l’onglet Requêtes affiche son activité.").size(10.0).color(MUTED));
            }
            ui.horizontal_wrapped(|ui| {
                for p in &enabled {
                    dot(ui, Color32::from_rgb(p.color[0], p.color[1], p.color[2]));
                    ui.label(RichText::new(&p.name).size(10.5).color(MUTED));
                    ui.add_space(12.0);
                }
            });
        });
    }
    fn history(&mut self, ui: &mut egui::Ui) {
        self.period_selector(ui);
        self.activity_chart(ui);
        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Journal de consommation").size(17.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+  Saisie manuelle").clicked() {
                    self.manual = Some(ManualEntry {
                        date: Local::now().format("%Y-%m-%d %H:%M").to_string(),
                        model: "manuel".into(),
                        ..Default::default()
                    });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Rechercher un modèle")
                        .desired_width(175.0),
                );
            });
        });
        ui.add_space(12.0);
        let events = self.selected_events();
        let total = events.len();
        let pages = total.div_ceil(50).max(1);
        let current_page = self.history_page.min(pages - 1);
        let start = current_page * 50;
        // Clone the visible page only so pagination controls can mutate the app.
        let visible: Vec<_> = events
            .iter()
            .rev()
            .skip(start)
            .take(50)
            .map(|e| (**e).clone())
            .collect();
        drop(events);
        self.history_page = current_page;
        card().show(ui,|ui| {
            egui::ScrollArea::horizontal().id_salt("history-table-scroll").show(ui,|ui| {
                egui::Grid::new("history-table").striped(true).spacing([17.0,13.0]).min_col_width(50.0).show(ui,|ui| {
                    for header in ["DATE LOCALE","FOURNISSEUR / MODÈLE","ENTRÉES","SORTIES","CACHE¹","TOTAL","REQUÊTES","COÛT / CRÉDITS"] {ui.label(RichText::new(header).size(9.5).color(MUTED));} ui.end_row();
                    for e in &visible {
                        ui.label(RichText::new(e.timestamp.with_timezone(&Local).format("%d/%m/%y  %H:%M").to_string()).size(11.0));
                        ui.vertical(|ui| {ui.label(RichText::new(self.config.providers.iter().find(|p|p.id==e.provider).map(|p|p.name.as_str()).unwrap_or(&e.provider)).size(11.0));ui.label(RichText::new(&e.model).size(10.0).color(MUTED));}).response.on_hover_text(&e.source);
                        ui.label(RichText::new(if e.tokens_reported{compact(e.input_tokens as f64)}else{"—".into()}).size(11.0)).on_hover_text(e.input_tokens.to_string());
                        ui.label(RichText::new(if e.tokens_reported{compact(e.output_tokens as f64)}else{"—".into()}).size(11.0)).on_hover_text(e.output_tokens.to_string());
                        ui.label(RichText::new(if e.tokens_reported{compact(e.cached_tokens as f64)}else{"—".into()}).size(11.0).color(MUTED));
                        ui.label(RichText::new(if e.tokens_reported{compact(e.total() as f64)}else{"—".into()}).size(11.0).color(ACCENT)).on_hover_text(if e.tokens_reported{e.total().to_string()}else{"Tokens non communiqués par le fournisseur".into()});
                        ui.label(RichText::new(e.request_count.to_string()).size(11.0));
                        ui.label(RichText::new(match (e.cost_usd,e.credits){(Some(c),_)=>format!("{c:.4} $"),(_,Some(c))=>format!("{c:.2} cr."),_=>"—".into()}).size(10.5));ui.end_row();
                    }
                });
            });
            if visible.is_empty(){ui.add_space(25.0);ui.label(RichText::new("Aucun relevé pour ce filtre. Connectez une source ou importez un historique.").color(MUTED));ui.add_space(25.0);}
            ui.add_space(10.0);ui.separator();ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{total} relevés · ¹ Le cache est inclus dans les entrées.")).size(10.5).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    if ui.add_enabled(self.history_page+1<pages,egui::Button::new("Suivant →")).clicked(){self.history_page+=1;}
                    ui.label(format!("{} / {pages}",self.history_page+1));
                    if ui.add_enabled(self.history_page>0,egui::Button::new("← Précédent")).clicked(){self.history_page-=1;}
                });
            });
        });
    }
    fn connections(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (index, p) in self.config.providers.iter().enumerate() {
                if ui.selectable_label(self.editor == index, &p.name).clicked() {
                    self.editor = index;
                }
            }
            if ui.button("+ Ajouter").clicked() {
                self.new_provider = Some(ProviderConfig::default());
            }
        });
        ui.add_space(18.0);
        if self.config.providers.is_empty() {
            return;
        }
        self.editor = self.editor.min(self.config.providers.len() - 1);
        let mut import = false;
        let mut remove = false;
        let id = self.config.providers[self.editor].id.clone();
        if self.key_for.as_deref() != Some(&id) {
            self.key_input.clear();
            self.key_for = Some(id);
        }
        let mut key_action: Option<Result<(), String>> = None;
        let p = &mut self.config.providers[self.editor];
        let previous_kind = p.kind;
        card().show(ui,|ui| {
            ui.horizontal(|ui| {ui.label(RichText::new("Source de consommation").size(17.0).strong());ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{self.dirty|=ui.checkbox(&mut p.enabled,"Activé").changed();});});
            ui.add_space(17.0);
            egui::Grid::new("provider-fields").num_columns(2).spacing([28.0,14.0]).show(ui,|ui| {
                ui.label("Nom du compte");self.dirty|=ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(350.0)).changed();ui.end_row();
                ui.label("Type de source");
                egui::ComboBox::from_id_salt("provider-kind").selected_text(p.kind.label()).width(330.0).show_ui(ui,|ui|{for kind in ProviderKind::ALL {self.dirty|=ui.selectable_value(&mut p.kind,kind,kind.label()).changed();}});ui.end_row();
                ui.label("Couleur");self.dirty|=ui.color_edit_button_srgb(&mut p.color).changed();ui.end_row();
            });
            if p.kind!=previous_kind {p.key_env=if p.kind==ProviderKind::OllamaCloud{"OLLAMA_API_KEY".into()}else{"ANTHROPIC_ADMIN_KEY".into()};self.dirty=true;}
            ui.add_space(17.0);ui.separator();ui.add_space(14.0);
            if matches!(p.kind,ProviderKind::Codex|ProviderKind::Claude|ProviderKind::ClaudeCode|ProviderKind::AnthropicApi|ProviderKind::OllamaCloud|ProviderKind::Antigravity|ProviderKind::Qoder) {
                self.dirty|=ui.checkbox(&mut p.online,"Récupérer les limites et crédits du compte").changed();ui.add_space(8.0);
                if matches!(p.kind,ProviderKind::AnthropicApi|ProviderKind::OllamaCloud) {
                    ui.label(RichText::new(if p.kind==ProviderKind::OllamaCloud{"Variable d’environnement contenant la clé API Ollama"}else{"Variable d’environnement contenant la clé Admin / Analytics"}).size(11.0).color(MUTED));
                    self.dirty|=ui.add(egui::TextEdit::singleline(&mut p.key_env).desired_width(450.0)).changed();
                    let source=if crate::keys::environment(&p.key_env).is_some(){Some(format!("Clé détectée dans {}",p.key_env))}
                        else if crate::keys::stored(&p.id).is_some(){Some("Clé enregistrée dans le coffre Windows".into())}
                        else if p.kind==ProviderKind::OllamaCloud{crate::keys::ollama_existing().map(|(_,source)|format!("Connexion détectée : {source}"))}else{None};
                    ui.label(RichText::new(source.unwrap_or_else(||"Clé non détectée : saisissez-la ci-dessous pour connecter ce compte.".into())).size(10.5).color(ACCENT));
                    ui.add_space(12.0);ui.label("Clé API alternative · coffre Windows");
                    ui.add(egui::TextEdit::singleline(&mut self.key_input).password(true).hint_text("Saisir une clé API").desired_width(450.0));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.key_input.trim().is_empty(),egui::Button::new("Enregistrer et connecter")).clicked(){key_action=Some(crate::keys::save(&p.id,&self.key_input));self.key_input.clear();p.online=true;}
                        if ui.small_button("Oublier la clé stockée").clicked(){key_action=Some(crate::keys::forget(&p.id));}
                    });
                    ui.label(RichText::new(if p.kind==ProviderKind::OllamaCloud{"Lecture de /api/balance et /api/usage. Détection automatique des clés OpenCode, sans recopier leur fichier."}else{"Historique distant de 90 jours, agrégé par jour UTC. Les clés sont conservées hors de la configuration."}).size(10.5).color(MUTED));
                } else if p.kind == ProviderKind::Qoder {
                    ui.label(RichText::new("Installez Qoder CLI et connectez-vous avec qodercli login. TokenScope utilise la session locale en lecture seule. Les crédits sont disponibles, pas l’historique de tokens.").size(10.5).color(MUTED));
                } else {
                    ui.label(RichText::new(if p.kind==ProviderKind::Antigravity{"Fichier de session Antigravity facultatif · vide = détection automatique"}else{"Fichier de session du client officiel"}).size(11.0).color(MUTED));
                    ui.horizontal(|ui| {
                        self.dirty|=ui.add(egui::TextEdit::singleline(&mut p.auth_path).desired_width((ui.available_width()-95.0).max(200.0))).changed();
                        if ui.button("Parcourir…").clicked()&& let Some(path)=rfd::FileDialog::new().add_filter("JSON",&["json"]).pick_file(){p.auth_path=path.display().to_string();self.dirty=true;}
                    });
                    ui.label(RichText::new("Lecture seule de la session existante. En cas d’expiration, reconnectez-vous dans le client officiel.").size(10.5).color(MUTED));
                    if p.kind==ProviderKind::Antigravity {
                        ui.label(RichText::new(crate::antigravity::connection_description(p)).size(10.5).color(ACCENT));
                        ui.label(RichText::new("Quotas de l’abonnement Google AI via Antigravity. La session est renouvelée si nécessaire par le client natif agy installé.").size(10.5).color(MUTED));
                        ui.label(RichText::new("Les fenêtres et groupes affichés sont ceux communiqués par Google ; les périodes manquantes restent inconnues.").size(10.5).color(MUTED));
                    }
                    if matches!(p.kind,ProviderKind::Claude|ProviderKind::ClaudeCode){ui.label(RichText::new("Claude et Claude Code utilisent le même quota d’abonnement. Les tokens du chat Claude nécessitent un import.").size(10.5).color(AMBER));}
                }
                ui.add_space(17.0);
            }
            ui.label(RichText::new("Journaux JSONL · un fichier ou dossier par ligne").size(11.5));ui.add_space(5.0);
            self.dirty|=ui.add(egui::TextEdit::multiline(&mut p.log_path).desired_width(f32::INFINITY).desired_rows(2).hint_text("C:\\chemin\\vers\\les\\journaux")).changed();
            ui.horizontal(|ui| {
                if ui.button("Choisir un dossier").clicked()&& let Some(path)=rfd::FileDialog::new().pick_folder(){if !p.log_path.trim().is_empty(){p.log_path.push('\n');}p.log_path.push_str(&path.display().to_string());self.dirty=true;}
                if ui.button("Choisir un fichier").clicked()&& let Some(path)=rfd::FileDialog::new().add_filter("JSONL",&["jsonl"]).pick_file(){if !p.log_path.trim().is_empty(){p.log_path.push('\n');}p.log_path.push_str(&path.display().to_string());self.dirty=true;}
                if ui.button("Importer CSV / JSONL").clicked(){import=true;}
            });ui.add_space(7.0);
            if p.kind==ProviderKind::Ollama {ui.label(RichText::new("Le collecteur intégré enregistre automatiquement les appels qui passent par lui. Activez-le dans Réglages.").size(11.0).color(ACCENT));}
            if p.kind==ProviderKind::Jsonl {ui.label(RichText::new("Format : id, provider, timestamp (RFC3339), model, input_tokens, output_tokens, cached_tokens, cost_usd, credits. Voir README et exemples.").size(10.5).color(MUTED));}
        });
        ui.add_space(18.0);
        card().show(ui,|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui|{ui.label(RichText::new("Budgets et limites personnalisés").size(17.0).strong());ui.label(RichText::new("Vos objectifs sont signalés comme personnels, indépendamment des quotas du compte.").size(10.5).color(MUTED));});
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{if ui.button("+ Ajouter une limite").clicked(){p.limits.push(Budget::default());self.dirty=true;}});
            });ui.add_space(16.0);
            let mut remove_limit=None;
            for (index,budget) in p.limits.iter_mut().enumerate() {
                ui.push_id(index,|ui| {
                    ui.horizontal(|ui| {
                        self.dirty|=ui.add(egui::TextEdit::singleline(&mut budget.name).desired_width(200.0)).changed();
                        self.dirty|=ui.add(egui::DragValue::new(&mut budget.amount).range(0.01..=1e15).speed(100.0)).changed();
                        egui::ComboBox::from_id_salt("unit").selected_text(budget.unit.label()).show_ui(ui,|ui|{for unit in [BudgetUnit::Tokens,BudgetUnit::Usd,BudgetUnit::Credits]{self.dirty|=ui.selectable_value(&mut budget.unit,unit,unit.label()).changed();}});
                        egui::ComboBox::from_id_salt("period").selected_text(budget.period.label()).show_ui(ui,|ui|{for period in [BudgetPeriod::FiveHours,BudgetPeriod::Day,BudgetPeriod::Week,BudgetPeriod::Month]{self.dirty|=ui.selectable_value(&mut budget.period,period,period.label()).changed();}});
                        if ui.small_button("Supprimer").clicked(){remove_limit=Some(index);}
                    });ui.add_space(5.0);
                    let mut manual=budget.manual_used.is_some();
                    if ui.checkbox(&mut manual,"Utiliser une consommation relevée manuellement jusqu’à la prochaine remise à zéro").changed(){budget.manual_used=if manual{Some(0.0)}else{None};budget.manual_reset=if manual{Some(Utc::now()+ChronoDuration::days(7))}else{None};self.reset_inputs.remove(&format!("{}:{index}",p.id));self.dirty=true;}
                    if let Some(used)=&mut budget.manual_used {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Consommé :");self.dirty|=ui.add(egui::DragValue::new(used).range(0.0..=1e15).speed(1.0)).changed();ui.label("Remise à zéro locale :");
                            let key=format!("{}:{index}",p.id);
                            let text=self.reset_inputs.entry(key).or_insert_with(||budget.manual_reset.unwrap_or(Utc::now()).with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string());
                            let response=ui.add(egui::TextEdit::singleline(text).desired_width(165.0).hint_text("AAAA-MM-JJ HH:MM"));
                            if response.changed(){match parse_local(text){Ok(time)=>{budget.manual_reset=Some(time);self.dirty=true;},Err(_)=>{ui.colored_label(RED,"Date invalide");}}}
                        });
                        if budget.manual_reset.is_some_and(|t|t<Utc::now()){ui.label(RichText::new("Relevé expiré : actualisez-le ou désactivez la saisie manuelle pour revenir au calcul local.").size(10.5).color(AMBER));}
                    }
                    if budget.period==BudgetPeriod::FiveHours {
                        ui.horizontal(|ui| {
                            ui.label("Ancrage local des fenêtres de 5 h :");
                            let key=format!("anchor:{}:{index}",p.id);
                            let text=self.reset_inputs.entry(key).or_insert_with(||budget.anchor.unwrap_or(Utc::now()).with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string());
                            if ui.add(egui::TextEdit::singleline(text).desired_width(165.0)).changed(){match parse_local(text){Ok(time)=>{budget.anchor=Some(time);self.dirty=true;},Err(_)=>{ui.colored_label(RED,"Date invalide");}}}
                        });
                        ui.label(RichText::new("Fenêtres personnelles fixes et répétées de 5 h. Le quota du fournisseur reste affiché séparément.").size(10.0).color(MUTED));
                    }
                    if budget.unit!=BudgetUnit::Tokens {ui.label(RichText::new("Calcul limité aux coûts ou crédits connus des relevés enregistrés.").size(10.0).color(AMBER));}
                    ui.add_space(10.0);ui.separator();ui.add_space(10.0);
                });
            }
            if let Some(index)=remove_limit {p.limits.remove(index);self.reset_inputs.clear();self.dirty=true;}
            if p.limits.is_empty(){ui.label(RichText::new("Aucun budget personnel. Ajoutez une limite en tokens, en USD ou en crédits.").color(MUTED));}
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("Enregistrer la configuration").color(BG))
                        .fill(ACCENT)
                        .min_size(vec2(235.0, 37.0)),
                )
                .clicked()
            {
                self.save();
            }
            if ui.button("Rétablir les chemins détectés").clicked() {
                let kind = self.config.providers[self.editor].kind;
                if let Some(default) = Config::default()
                    .providers
                    .into_iter()
                    .find(|p| p.kind == kind)
                {
                    self.config.providers[self.editor].log_path = default.log_path;
                    self.config.providers[self.editor].auth_path = default.auth_path;
                    self.dirty = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Retirer ce fournisseur").clicked() {
                    remove = true;
                }
            });
        });
        if import {
            let id = self.config.providers[self.editor].id.clone();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Historique", &["csv", "jsonl"])
                .pick_file()
            {
                match self.store.import(&path, &id) {
                    Ok(count) => {
                        self.notify(
                            format!("{count} relevés importés. Les doublons ont été ignorés."),
                            false,
                        );
                        self.refresh();
                    }
                    Err(e) => self.notify(e, true),
                }
            }
        }
        if remove {
            let id = self.config.providers.remove(self.editor).id;
            self.filter = self.filter.take().filter(|filter| filter != &id);
            self.editor = 0;
            self.dirty = true;
        }
        if let Some(result) = key_action {
            match result {
                Ok(()) => self.save(),
                Err(e) => self.notify(e, true),
            }
        }
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui,|ui| {
            section(ui,"Actualisation et alertes","Les comptes à rebours sont mis à jour chaque seconde.");ui.add_space(20.0);
            egui::Grid::new("settings-fields").spacing([35.0,18.0]).show(ui,|ui| {
                ui.label("Analyse des journaux");self.dirty|=ui.add(egui::DragValue::new(&mut self.config.refresh_seconds).range(15..=3600).suffix(" s").speed(5)).changed();ui.end_row();
                ui.label("Alerte à partir de");self.dirty|=ui.add(egui::Slider::new(&mut self.config.alert_percent,1.0..=100.0).suffix(" %")).changed();ui.end_row();
            });ui.add_space(15.0);
            ui.label(RichText::new("Les quotas distants sont interrogés toutes les 3 minutes. Actualiser peut relancer une lecture après 20 secondes.").size(11.0).color(MUTED));
            ui.label(RichText::new("Les jauges passent en orange au seuil d’alerte, puis en rouge à 100 %. Une échéance dépassée attend confirmation du fournisseur.").size(11.0).color(MUTED));
        });
        ui.add_space(18.0);
        card().show(ui,|ui| {
            section(ui,"Collecteur Llama / Ollama","Mesurez vos prochains appels locaux, y compris les réponses en streaming.");ui.add_space(18.0);
            self.dirty|=ui.checkbox(&mut self.config.proxy_enabled,"Activer le collecteur natif Ollama").changed();ui.add_space(12.0);
            egui::Grid::new("proxy-fields").min_col_width(220.0).spacing([35.0,14.0]).show(ui,|ui| {
                ui.label("Serveur Ollama local");self.dirty|=ui.add(egui::TextEdit::singleline(&mut self.config.ollama_url).desired_width(300.0)).changed();ui.end_row();
                ui.label("Port du collecteur");self.dirty|=ui.add(egui::DragValue::new(&mut self.config.proxy_port).range(1024..=65535).speed(1)).changed();ui.end_row();
            });ui.add_space(15.0);
            if let Some(proxy)=&self.proxy {pill(ui,&proxy.status(),ACCENT);}else{ui.label(RichText::new("Collecteur arrêté").color(MUTED));}
            ui.add_space(12.0);
            ui.label("Dans votre client Ollama, utilisez cette adresse de serveur :");
            ui.horizontal(|ui| {let address=format!("http://127.0.0.1:{}",self.config.proxy_port);ui.code(&address);if ui.small_button("Copier").clicked(){ui.ctx().copy_text(address);}});
            ui.add_space(9.0);
            ui.label(RichText::new("Ollama doit être démarré. Les appels directs au port 11434 ne sont pas comptés. Les historiques antérieurs nécessitent un import.").size(11.0).color(MUTED));
            ui.label(RichText::new("Routes : /api/chat, /api/generate et /v1/chat/completions. Pour le streaming compatible OpenAI, demandez stream_options.include_usage=true.").size(10.5).color(MUTED));
        });
        ui.add_space(18.0);
        card().show(ui,|ui| {
            section(ui,"Données de l’application","Configuration, relevés importés et cache des dernières limites.");ui.add_space(12.0);
            ui.label(RichText::new(self.store.root.display().to_string()).monospace().size(10.5).color(MUTED));
            ui.add_space(9.0);if ui.button("Ouvrir le dossier des données").clicked(){open_folder(&self.store.root);}
            ui.add_space(10.0);
            ui.label(RichText::new("Les identifiants existants sont lus en mémoire pour consulter les quotas de leurs services. Les conversations et les clés ne sont pas recopiées dans les données TokenScope.").size(11.0).color(MUTED));
            ui.label(RichText::new("Les points de lecture des quotas Codex et Claude peuvent évoluer. Si une connexion échoue, le dernier relevé reste daté et les imports restent disponibles.").size(10.5).color(MUTED));
        });
        ui.add_space(18.0);
        if ui
            .add(
                egui::Button::new(RichText::new("Enregistrer les réglages").color(BG))
                    .fill(ACCENT)
                    .min_size(vec2(220.0, 38.0)),
            )
            .clicked()
        {
            self.save();
        }
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(mut p) = self.new_provider.take() {
            let mut open = true;
            let mut create = false;
            let mut cancel = false;
            egui::Window::new("Ajouter un fournisseur")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(10.0);
                    ui.label("Nom du compte");
                    ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(f32::INFINITY));
                    ui.add_space(13.0);
                    ui.label("Source");
                    egui::ComboBox::from_id_salt("new-kind")
                        .selected_text(p.kind.label())
                        .width(360.0)
                        .show_ui(ui, |ui| {
                            for kind in ProviderKind::ALL {
                                ui.selectable_value(&mut p.kind, kind, kind.label());
                            }
                        });
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if ui.button("Annuler").clicked() {
                            cancel = true;
                        }
                        if ui
                            .add_enabled(
                                !p.name.trim().is_empty(),
                                egui::Button::new("Ajouter et configurer")
                                    .fill(ACCENT)
                                    .stroke(Stroke::NONE),
                            )
                            .clicked()
                        {
                            create = true;
                        }
                    });
                    ui.add_space(8.0);
                });
            if create {
                if let Some(default) = Config::default()
                    .providers
                    .into_iter()
                    .find(|d| d.kind == p.kind)
                {
                    p.log_path = default.log_path;
                    p.auth_path = default.auth_path;
                    p.online = default.online;
                    p.color = default.color;
                    p.key_env = default.key_env;
                }
                if p.kind == ProviderKind::Qoder { p.online = true; }
                self.config.providers.push(p);
                self.editor = self.config.providers.len() - 1;
                self.page = Page::Connections;
                self.dirty = true;
                self.filter = None;
            } else if open && !cancel {
                self.new_provider = Some(p);
            }
        }
        if let Some(mut entry) = self.manual.take() {
            let mut open = true;
            let mut save = false;
            let mut cancel = false;
            egui::Window::new("Ajouter un relevé de consommation")
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    egui::Grid::new("manual-entry")
                        .spacing([25.0, 13.0])
                        .show(ui, |ui| {
                            ui.label("Fournisseur");
                            egui::ComboBox::from_id_salt("manual-provider")
                                .selected_text(
                                    self.config
                                        .providers
                                        .get(entry.provider)
                                        .map(|p| p.name.as_str())
                                        .unwrap_or("Choisir"),
                                )
                                .show_ui(ui, |ui| {
                                    for (index, p) in self.config.providers.iter().enumerate() {
                                        ui.selectable_value(&mut entry.provider, index, &p.name);
                                    }
                                });
                            ui.end_row();
                            ui.label("Modèle");
                            ui.text_edit_singleline(&mut entry.model);
                            ui.end_row();
                            ui.label("Date et heure locales");
                            ui.add(
                                egui::TextEdit::singleline(&mut entry.date)
                                    .hint_text("AAAA-MM-JJ HH:MM"),
                            );
                            ui.end_row();
                            ui.label("Tokens d’entrée (cache inclus)");
                            ui.add(
                                egui::DragValue::new(&mut entry.input)
                                    .range(0..=u64::MAX)
                                    .speed(100),
                            );
                            ui.end_row();
                            ui.label("Tokens de sortie");
                            ui.add(
                                egui::DragValue::new(&mut entry.output)
                                    .range(0..=u64::MAX)
                                    .speed(100),
                            );
                            ui.end_row();
                            ui.label("Dont cache lu");
                            ui.add(
                                egui::DragValue::new(&mut entry.cached)
                                    .range(0..=u64::MAX)
                                    .speed(100),
                            );
                            ui.end_row();
                            ui.checkbox(&mut entry.has_cost, "Coût connu en USD");
                            ui.add_enabled(
                                entry.has_cost,
                                egui::DragValue::new(&mut entry.cost)
                                    .range(0.0..=1e9)
                                    .speed(0.01)
                                    .suffix(" $"),
                            );
                            ui.end_row();
                            ui.checkbox(&mut entry.has_credits, "Crédits consommés connus");
                            ui.add_enabled(
                                entry.has_credits,
                                egui::DragValue::new(&mut entry.credits)
                                    .range(0.0..=1e12)
                                    .speed(1),
                            );
                            ui.end_row();
                        });
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if ui.button("Annuler").clicked() {
                            cancel = true;
                        }
                        if ui.button("Enregistrer le relevé").clicked() {
                            save = true;
                        }
                    });
                });
            if save {
                let result = parse_local(&entry.date).and_then(|timestamp| {
                    if timestamp > Utc::now() {
                        return Err(
                            "Le relevé doit être daté dans le passé ou à l’instant présent.".into(),
                        );
                    }
                    let p = self
                        .config
                        .providers
                        .get(entry.provider)
                        .ok_or("Choisissez un fournisseur.")?;
                    self.store.append_event(
                        &UsageEvent {
                            id: format!("manual:{}", uuid::Uuid::new_v4()),
                            provider: p.id.clone(),
                            timestamp,
                            model: entry.model.clone(),
                            input_tokens: entry.input,
                            output_tokens: entry.output,
                            cached_tokens: entry.cached,
                            cache_write_tokens: 0,
                            cost_usd: entry.has_cost.then_some(entry.cost),
                            credits: entry.has_credits.then_some(entry.credits),
                            source: "Saisie manuelle".into(),
                            tokens_reported: true,
                            request_count: 1,
                        },
                        false,
                    )
                });
                match result {
                    Ok(()) => {
                        self.notify("Relevé enregistré.".into(), false);
                        self.refresh();
                    }
                    Err(e) => {
                        self.notify(e, true);
                        self.manual = Some(entry);
                    }
                }
            } else if open && !cancel {
                self.manual = Some(entry);
            }
        }
    }
}

impl eframe::App for TokenScope {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                WorkerMessage::Started => self.busy = true,
                WorkerMessage::Finished(snapshot) => {
                    self.snapshot = snapshot;
                    self.busy = false;
                }
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5)) {
            self.refresh();
        }
        self.sidebar(ctx);
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, _, time)| time.elapsed().as_secs() > 12)
        {
            self.toast = None;
        }
        if let Some((message, error, _)) = &self.toast {
            egui::TopBottomPanel::bottom("notification")
                .frame(
                    egui::Frame::new()
                        .fill(if *error {
                            Color32::from_rgb(67, 33, 40)
                        } else {
                            Color32::from_rgb(26, 51, 46)
                        })
                        .inner_margin(12),
                )
                .show(ctx, |ui| {
                    ui.label(RichText::new(message).color(if *error { RED } else { ACCENT }));
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(26))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(("page-scroll", self.page as u8))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.header(ui);
                        match self.page {
                            Page::Overview => self.overview(ui),
                            Page::History => self.history(ui),
                            Page::Connections => self.connections(ui),
                            Page::Settings => self.settings(ui),
                        }
                        ui.add_space(25.0);
                    });
            });
        self.dialogs(ctx);
        if self.capture.is_some()
            && !self.capture_requested
            && (!self.busy || self.launched.elapsed().as_secs() > 90)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.capture_requested = true;
        }
        if self.capture_requested {
            let screenshot = ctx.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = screenshot
                && let Some(path) = self.capture.take()
            {
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(error) = image::save_buffer(
                    &path,
                    &bytes,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                ) {
                    self.notify(format!("Capture impossible : {error}"), true);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.faint_bg_color = Color32::from_rgb(23, 29, 38);
    visuals.extreme_bg_color = Color32::from_rgb(14, 18, 25);
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(30, 37, 49);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(30, 37, 49);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 56, 69);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(43, 56, 69);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.5));
    visuals.selection.bg_fill = Color32::from_rgb(38, 77, 68);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.hyperlink_color = ACCENT;
    ctx.set_visuals(visuals);
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(bytes) = std::fs::read("C:/Windows/Fonts/segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "segoe".into());
    }
    ctx.set_fonts(fonts);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(9.0, 7.0);
        s.spacing.button_padding = vec2(11.0, 7.0);
        s.text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.0));
        s.text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(12.0));
    });
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(12.0)
        .inner_margin(18)
}
fn section(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(17.0).strong());
    ui.label(RichText::new(subtitle).size(10.5).color(MUTED));
}
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(7.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(9.0).color(color));
        });
}
fn metric(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    caption: &str,
    color: Color32,
) -> egui::Response {
    card()
        .show(ui, |ui| {
            ui.set_min_height(77.0);
            ui.label(RichText::new(label).size(9.0).color(MUTED));
            ui.add_space(7.0);
            ui.label(RichText::new(value).size(25.0).strong().color(color));
            ui.add_space(3.0);
            ui.label(RichText::new(caption).size(10.0).color(MUTED));
        })
        .response
}
fn logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 28.0), egui::Sense::hover());
    for (i, height) in [11.0, 18.0, 25.0].into_iter().enumerate() {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + i as f32 * 8.0, rect.bottom() - height),
                vec2(5.0, height),
            ),
            2.0,
            ACCENT,
        );
    }
}
fn provider_icon(ui: &mut egui::Ui, kind: ProviderKind, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 9.0, color.gamma_multiply(0.12));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        match kind {
            ProviderKind::Codex => ">_",
            ProviderKind::Claude => "✳",
            ProviderKind::ClaudeCode => "{ }",
            ProviderKind::Antigravity => "G",
            ProviderKind::Qoder => "Q",
            ProviderKind::Ollama | ProviderKind::OllamaCloud => "L",
            _ => "+",
        },
        FontId::proportional(17.0),
        color,
    );
}
fn short_countdown(time: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (time - now).num_seconds().max(0);
    if seconds >= 86400 {
        format!("{} j {} h", seconds / 86400, (seconds % 86400) / 3600)
    } else {
        format!("{} h {:02} min", seconds / 3600, (seconds % 3600) / 60)
    }
}
fn quota_row(
    ui: &mut egui::Ui,
    q: &Quota,
    color: Color32,
    now: DateTime<Utc>,
    threshold: f64,
    personal: bool,
    observed: Option<DateTime<Utc>>,
) {
    let expired = q.resets_at.is_some_and(|reset| reset <= now);
    let stale = observed.is_some_and(|t| (now - t).num_minutes() > 15);
    let color = if q.used_percent >= 100.0 {
        RED
    } else if expired || stale || q.used_percent >= threshold {
        AMBER
    } else {
        color
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&q.name)
                .size(11.5)
                .color(if personal { MUTED } else { TEXT }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(percent(q.used_percent))
                    .size(13.0)
                    .strong()
                    .color(color),
            );
        });
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 4.0, Color32::from_rgb(38, 45, 58));
    let width = rect.width() * (q.used_percent / 100.0).clamp(0.0, 1.0) as f32;
    if width > 0.0 {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(width, rect.height())),
            4.0,
            color,
        );
    }
    ui.add_space(1.0);
    ui.horizontal_wrapped(|ui| {
        let text = if expired {
            "À confirmer auprès du compte".into()
        } else if let (Some(used), Some(limit)) = (q.used, q.limit) {
            if q.unit == "tokens" {
                format!("{} / {} {}", compact(used), compact(limit), q.unit)
            } else {
                format!("{used:.2} / {limit:.2} {}", q.unit)
            }
        } else {
            format!("{} disponibles", percent((100.0 - q.used_percent).max(0.0)))
        };
        ui.label(RichText::new(text).size(9.5).color(MUTED));
        if !expired && (q.resets_at.is_some() || q.unit != "crédits") {
            ui.label(
                RichText::new(format!("·  ↻ {}", countdown(q.resets_at, now)))
                    .size(9.5)
                    .color(if stale { AMBER } else { MUTED }),
            )
            .on_hover_text(
                q.resets_at
                    .map(|t| {
                        format!(
                            "Réinitialisation : {} (heure locale)",
                            t.with_timezone(&Local).format("%d/%m/%Y à %H:%M:%S")
                        )
                    })
                    .unwrap_or_else(|| {
                        "Le fournisseur n’a pas encore communiqué d’échéance.".into()
                    }),
            );
        }
        if personal {
            ui.label(RichText::new("PERSONNEL").size(8.0).color(MUTED));
        }
    });
    if let Some(reset) = q.resets_at
        && !expired
    {
        ui.label(
            RichText::new(format!(
                "Remise à zéro le {}",
                reset.with_timezone(&Local).format("%d/%m à %H:%M")
            ))
            .size(9.5)
            .color(MUTED),
        );
    }
    ui.add_space(12.0);
}
fn percent(value: f64) -> String {
    if (value - value.round()).abs() < 0.005 {
        format!("{value:.0} %")
    } else {
        format!("{value:.2} %")
    }
}
fn parse_local(text: &str) -> Result<DateTime<Utc>, String> {
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M")
        .map_err(|_| "Date attendue : AAAA-MM-JJ HH:MM (heure locale).".to_string())?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| "Cette heure locale est ambiguë ou n’existe pas.".into())
}
fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
