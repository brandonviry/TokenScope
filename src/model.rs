use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ProviderKind {
    Codex,
    Claude,
    ClaudeCode,
    Antigravity,
    Qoder,
    OllamaCloud,
    Ollama,
    #[default]
    Jsonl,
    AnthropicApi,
}

impl ProviderKind {
    pub const ALL: [Self; 9] = [
        Self::Codex,
        Self::Claude,
        Self::ClaudeCode,
        Self::Antigravity,
        Self::Qoder,
        Self::OllamaCloud,
        Self::Ollama,
        Self::Jsonl,
        Self::AnthropicApi,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude (abonnement)",
            Self::ClaudeCode => "Claude Code",
            Self::Antigravity => "Google Antigravity / Gemini",
            Self::Qoder => "Qoder CLI",
            Self::OllamaCloud => "Ollama Cloud (clé API)",
            Self::Ollama => "Ollama local (collecteur optionnel)",
            Self::Jsonl => "Fournisseur personnalisé (JSONL)",
            Self::AnthropicApi => "Anthropic API (organisation)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub enabled: bool,
    pub online: bool,
    pub log_path: String,
    pub auth_path: String,
    pub key_env: String,
    pub color: [u8; 3],
    pub limits: Vec<Budget>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Nouveau fournisseur".into(),
            kind: ProviderKind::Jsonl,
            enabled: true,
            online: false,
            log_path: String::new(),
            auth_path: String::new(),
            key_env: "ANTHROPIC_ADMIN_KEY".into(),
            color: [154, 141, 249],
            limits: vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "legacy_schema_version")]
    pub schema_version: u32,
    pub providers: Vec<ProviderConfig>,
    pub refresh_seconds: u64,
    pub alert_percent: f64,
    pub proxy_enabled: bool,
    pub proxy_port: u16,
    pub ollama_url: String,
}
fn legacy_schema_version() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        let home = directories::UserDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .unwrap_or_default();
        let codex = std::env::var_os("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let base = |id: &str, name: &str, kind, color, log: String, auth: String| ProviderConfig {
            id: id.into(),
            name: name.into(),
            kind,
            color,
            log_path: log,
            auth_path: auth,
            key_env: if kind == ProviderKind::OllamaCloud {
                "OLLAMA_API_KEY".into()
            } else {
                "ANTHROPIC_ADMIN_KEY".into()
            },
            online: matches!(
                kind,
                ProviderKind::Codex
                    | ProviderKind::Claude
                    | ProviderKind::ClaudeCode
                    | ProviderKind::Antigravity
                    | ProviderKind::Qoder
                    | ProviderKind::OllamaCloud
            ),
            ..Default::default()
        };
        Self {
            schema_version: 3,
            providers: vec![
                base(
                    "codex",
                    "Codex",
                    ProviderKind::Codex,
                    [100, 222, 186],
                    codex.join("sessions").display().to_string(),
                    codex.join("auth.json").display().to_string(),
                ),
                base(
                    "claude",
                    "Claude",
                    ProviderKind::Claude,
                    [226, 166, 128],
                    String::new(),
                    claude.join(".credentials.json").display().to_string(),
                ),
                base(
                    "claude-code",
                    "Claude Code",
                    ProviderKind::ClaudeCode,
                    [178, 157, 246],
                    claude.join("projects").display().to_string(),
                    claude.join(".credentials.json").display().to_string(),
                ),
                base(
                    "antigravity",
                    "Antigravity · Gemini",
                    ProviderKind::Antigravity,
                    [138, 180, 248],
                    String::new(),
                    String::new(),
                ),
                base(
                    "qoder",
                    "Qoder",
                    ProviderKind::Qoder,
                    [120, 174, 250],
                    String::new(),
                    String::new(),
                ),
                base(
                    "ollama-cloud",
                    "Ollama Cloud",
                    ProviderKind::OllamaCloud,
                    [120, 174, 250],
                    String::new(),
                    String::new(),
                ),
            ],
            refresh_seconds: 60,
            alert_percent: 80.0,
            proxy_enabled: false,
            proxy_port: 11435,
            ollama_url: "http://127.0.0.1:11434".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UsageEvent {
    pub id: String,
    pub provider: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default = "default_model")]
    pub model: String,
    /// All input tokens, including cache reads and cache writes.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Subset of input_tokens. Never added to total a second time.
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub credits: Option<f64>,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "true_value")]
    pub tokens_reported: bool,
    #[serde(default = "one_request")]
    pub request_count: u64,
}
fn true_value() -> bool {
    true
}
fn one_request() -> u64 {
    1
}
fn default_model() -> String {
    "inconnu".into()
}
fn default_source() -> String {
    "Import".into()
}

impl UsageEvent {
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
    pub fn valid(&self) -> bool {
        self.cached_tokens <= self.input_tokens
            && self.cache_write_tokens <= self.input_tokens
            && self.cost_usd.is_none_or(|v| v.is_finite() && v >= 0.0)
            && self.credits.is_none_or(|v| v.is_finite() && v >= 0.0)
            && !self.id.is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Quota {
    pub name: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub window_seconds: Option<i64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub unit: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    pub id: String,
    pub quotas: Vec<Quota>,
    pub observed_at: Option<DateTime<Utc>>,
    pub source: String,
    pub status: String,
    pub plan: Option<String>,
    pub credit_balance: Option<String>,
    pub unlimited_credits: bool,
    pub shared_group: Option<String>,
    pub live: bool,
    #[serde(default)]
    pub usage_note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub events: Vec<UsageEvent>,
    pub providers: Vec<ProviderSnapshot>,
    pub warnings: Vec<String>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub files_scanned: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum BudgetUnit {
    #[default]
    Tokens,
    Usd,
    Credits,
}
impl BudgetUnit {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Usd => "USD",
            Self::Credits => "crédits",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum BudgetPeriod {
    FiveHours,
    Day,
    #[default]
    Week,
    Month,
}
impl BudgetPeriod {
    pub fn label(self) -> &'static str {
        match self {
            Self::FiveHours => "Fenêtre de 5 h",
            Self::Day => "Jour",
            Self::Week => "Semaine",
            Self::Month => "Mois",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub name: String,
    pub amount: f64,
    pub unit: BudgetUnit,
    pub period: BudgetPeriod,
    /// Anchor for fixed, repeating 5h periods or a billing cycle (local day/time).
    pub anchor: Option<DateTime<Utc>>,
    /// Optional externally observed consumption. An expired observation stays
    /// visible until replaced; never invent a zero after its reset date.
    pub manual_used: Option<f64>,
    pub manual_reset: Option<DateTime<Utc>>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            name: "Budget personnel".into(),
            amount: 1_000_000.0,
            unit: BudgetUnit::Tokens,
            period: BudgetPeriod::Week,
            anchor: Some(local_midnight(Local::now().date_naive())),
            manual_used: None,
            manual_reset: None,
        }
    }
}

pub fn local_midnight(date: NaiveDate) -> DateTime<Utc> {
    // Try the following hours if a local clock change skips midnight.
    for hour in 0..24 {
        if let Some(dt) = Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .earliest()
        {
            return dt.with_timezone(&Utc);
        }
    }
    date.and_hms_opt(0, 0, 0).unwrap().and_utc()
}

pub fn period_bounds(
    period: BudgetPeriod,
    now: DateTime<Utc>,
    anchor: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let date = now.with_timezone(&Local).date_naive();
    match period {
        BudgetPeriod::FiveHours => {
            let a = anchor
                .unwrap_or_else(|| local_midnight(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()));
            let step = 5 * 3600;
            let start = a + Duration::seconds((now - a).num_seconds().div_euclid(step) * step);
            (start, start + Duration::hours(5))
        }
        BudgetPeriod::Day => (
            local_midnight(date),
            local_midnight(date + Duration::days(1)),
        ),
        BudgetPeriod::Week => {
            let d = date - Duration::days(date.weekday().num_days_from_monday() as i64);
            (local_midnight(d), local_midnight(d + Duration::days(7)))
        }
        BudgetPeriod::Month => {
            let start = NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap();
            let next = if date.month() == 12 {
                NaiveDate::from_ymd_opt(date.year() + 1, 1, 1).unwrap()
            } else {
                NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1).unwrap()
            };
            (local_midnight(start), local_midnight(next))
        }
    }
}

impl Budget {
    pub fn quota(&self, provider: &str, events: &[UsageEvent], now: DateTime<Utc>) -> Quota {
        let (start, end) = period_bounds(self.period, now, self.anchor);
        let observed = self.manual_used.zip(self.manual_reset);
        let used = observed.map(|(v, _)| v).unwrap_or_else(|| {
            events
                .iter()
                .filter(|e| e.provider == provider && e.timestamp >= start && e.timestamp < end)
                .map(|e| match self.unit {
                    BudgetUnit::Tokens => e.total() as f64,
                    BudgetUnit::Usd => e.cost_usd.unwrap_or(0.0),
                    BudgetUnit::Credits => e.credits.unwrap_or(0.0),
                })
                .sum()
        });
        Quota {
            name: format!("{} · {}", self.name, self.period.label()),
            used_percent: if self.amount > 0.0 {
                used / self.amount * 100.0
            } else {
                0.0
            },
            resets_at: Some(observed.map(|(_, reset)| reset).unwrap_or(end)),
            window_seconds: Some((end - start).num_seconds()),
            used: Some(used),
            limit: Some(self.amount),
            unit: self.unit.label().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewPeriod {
    Today,
    #[default]
    Week,
    Month,
    All,
}
impl ViewPeriod {
    pub const ALL: [Self; 4] = [Self::Today, Self::Week, Self::Month, Self::All];
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Aujourd’hui",
            Self::Week => "Cette semaine",
            Self::Month => "Ce mois",
            Self::All => "Tout",
        }
    }
    pub fn contains(self, time: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        if time > now {
            return false;
        }
        let period = match self {
            Self::Today => BudgetPeriod::Day,
            Self::Week => BudgetPeriod::Week,
            Self::Month => BudgetPeriod::Month,
            Self::All => return true,
        };
        let (start, end) = period_bounds(period, now, None);
        time >= start && time < end
    }
}

pub fn compact(n: f64) -> String {
    if n >= 1_000_000_000.0 {
        format!("{:.2} Md", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("{:.2} M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1} k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}
pub fn countdown(reset: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(reset) = reset else {
        return "Au prochain usage".into();
    };
    let seconds = (reset - now).num_seconds();
    if seconds <= 0 {
        return "Échéance passée · à confirmer".into();
    }
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let minutes = (seconds % 3600) / 60;
    if days > 0 {
        format!("{days} j {hours:02} h {minutes:02} min")
    } else if hours > 0 {
        format!("{hours} h {minutes:02} min {s:02} s", s = seconds % 60)
    } else {
        format!("{minutes} min {s:02} s", s = seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_observation_expiring_is_not_silently_zeroed() {
        let now = Utc::now();
        let budget = Budget {
            amount: 100.0,
            manual_used: Some(85.0),
            manual_reset: Some(now - Duration::minutes(1)),
            ..Default::default()
        };
        let quota = budget.quota("p", &[], now);
        assert_eq!(quota.used_percent, 85.0);
        assert!(quota.resets_at.unwrap() < now);
    }
    #[test]
    fn fixed_five_hour_windows_continue_across_midnight() {
        let anchor = Local
            .with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let before = Local
            .with_ymd_and_hms(2026, 11, 1, 23, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let after = before + Duration::hours(1);
        assert_eq!(
            period_bounds(BudgetPeriod::FiveHours, before, Some(anchor)),
            period_bounds(BudgetPeriod::FiveHours, after, Some(anchor))
        );
    }
    #[test]
    fn expired_quota_is_not_assumed_reset() {
        let now = Utc::now();
        assert!(countdown(Some(now - Duration::seconds(1)), now).contains("confirmer"));
    }
    #[test]
    fn monthly_period_handles_december() {
        let now = Local
            .with_ymd_and_hms(2026, 12, 15, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let (a, b) = period_bounds(BudgetPeriod::Month, now, None);
        assert_eq!(a.with_timezone(&Local).day(), 1);
        assert_eq!(b.with_timezone(&Local).year(), 2027);
    }
    #[test]
    fn five_hour_anchor_and_week_monday() {
        let now = Utc::now();
        let anchor = now - Duration::hours(6);
        let (a, b) = period_bounds(BudgetPeriod::FiveHours, now, Some(anchor));
        assert_eq!((b - a).num_hours(), 5);
        assert!(a <= now && b > now);
        let (a, _) = period_bounds(BudgetPeriod::Week, now, None);
        assert_eq!(a.with_timezone(&Local).weekday(), chrono::Weekday::Mon);
    }
}
