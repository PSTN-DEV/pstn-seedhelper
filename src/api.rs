use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const HUB_API: &str = "https://api.hub.pstnsquad.ru/api/v1/public";
const SEEDING_API: &str = "https://seeding.pstnsquad.ru/api";
const APP_ARCH: &str = env!("APP_ARCH"); // baked in at compile time by build.rs

pub const SERVER_TAGS: [(u8, &str); 4] = [(1, "A"), (2, "B"), (3, "C"), (4, "D")];
pub const SERVER_NAMES: [(u8, &str); 4] = [
    (1, "PSTN #1 Первый Сервер"),
    (2, "PSTN #2 Инвага"),
    (3, "PSTN #3 ВС РФ vs ВСУ 24/7"),
    (4, "PSTN #4 Все режимы"),
];

/// Strips " | pstnsquad.ru" (and similar trailing domain segments) from server names.
pub fn clean_name(name: &str) -> &str {
    if let Some(i) = name.find("| pstnsquad") {
        name[..i].trim_end()
    } else {
        name
    }
}

pub fn tag_for(server_num: u8) -> Option<&'static str> {
    SERVER_TAGS.iter().find(|(n, _)| *n == server_num).map(|(_, t)| *t)
}

fn num_from_tag(tag: &str) -> Option<u8> {
    SERVER_TAGS.iter().find(|(_, t)| *t == tag).map(|(n, _)| *n)
}

pub fn name_for(server_num: u8) -> &'static str {
    SERVER_NAMES
        .iter()
        .find(|(n, _)| *n == server_num)
        .map(|(_, name)| *name)
        .unwrap_or("Unknown")
}

// ── API response types ───────────────────────────────────────────────────────

/// Data for one server returned by /servers.
/// Field names must match the actual JSON keys from the API.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ServerData {
    pub state: String,
    #[serde(default)]
    pub players: u32,
    #[serde(default = "default_max")]
    pub max_players: u32,
    #[serde(default)]
    pub queue: u32,
    #[serde(default)]
    pub layer: String,
    #[serde(default)]
    pub team1_faction: String,
    #[serde(default)]
    pub team2_faction: String,
}

fn default_max() -> u32 { 100 }

impl ServerData {
    pub fn is_online(&self) -> bool {
        self.state == "online"
    }
}

#[derive(Deserialize)]
struct ServersResponse {
    servers: HashMap<String, ServerData>,
}

#[derive(Deserialize)]
struct CheckResponse {
    connected: bool,
}

#[derive(Deserialize)]
struct VersionResponse {
    version: String,
}

// ── HubApi ───────────────────────────────────────────────────────────────────

pub struct HubApi {
    client: Client,
}

impl HubApi {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client init failed");
        Self { client }
    }

    /// Fetch status for all servers in one request.
    pub async fn get_all_servers(&self) -> Result<HashMap<String, ServerData>> {
        let resp = self
            .client
            .get(format!("{HUB_API}/servers"))
            .send()
            .await
            .context("GET /servers")?
            .error_for_status()
            .context("GET /servers status")?
            .json::<ServersResponse>()
            .await
            .context("GET /servers parse")?;
        Ok(resp.servers)
    }

    /// Get status for a single server by number (1-4).
    pub async fn get_server(&self, server_num: u8) -> Result<ServerData> {
        let tag = tag_for(server_num).ok_or_else(|| anyhow!("unknown server {server_num}"))?;
        let all = self.get_all_servers().await?;
        all.into_iter()
            .find(|(k, _)| k == tag)
            .map(|(_, v)| v)
            .ok_or_else(|| anyhow!("server {tag} not in response"))
    }

    /// POST /join-server — returns a steam://joinlobby/... URL.
    pub async fn join_server(&self, server_num: u8) -> Result<String> {
        #[derive(Serialize)]
        struct Req { server_id: u8 }
        #[derive(Deserialize)]
        struct Resp { #[serde(rename = "connectUrl")] connect_url: String }

        let resp = self.client
            .post(format!("{SEEDING_API}/join-server"))
            .json(&Req { server_id: server_num })
            .send()
            .await
            .context("POST /join-server")?
            .error_for_status()
            .context("POST /join-server status")?
            .json::<Resp>()
            .await
            .context("POST /join-server parse")?;
        Ok(resp.connect_url)
    }

    /// Check if a Steam player is connected to a server.
    pub async fn check_player(&self, steam_id: &str, server_num: u8) -> Result<bool> {
        let tag = tag_for(server_num).ok_or_else(|| anyhow!("unknown server {server_num}"))?;
        let resp = self
            .client
            .get(format!("{HUB_API}/check"))
            .query(&[("steamid", steam_id), ("server", tag)])
            .send()
            .await
            .context("GET /check")?
            .error_for_status()
            .context("GET /check status")?
            .json::<CheckResponse>()
            .await
            .context("GET /check parse")?;
        Ok(resp.connected)
    }

    /// Fetch the remote seed order (server numbers in priority order).
    pub async fn get_seed_order(&self) -> Result<Vec<u8>> {
        #[derive(Deserialize)]
        struct SeedResp { order: Vec<String> }

        let resp = self
            .client
            .get(format!("{HUB_API}/seed"))
            .send()
            .await
            .context("GET /seed")?
            .error_for_status()
            .context("GET /seed status")?
            .json::<SeedResp>()
            .await
            .context("GET /seed parse")?;
        Ok(resp.order.iter().filter_map(|t| num_from_tag(t)).collect())
    }

    /// Fetch the latest released version string for the updater.
    pub async fn get_latest_version(&self) -> Result<String> {
        let resp = self
            .client
            .get(format!("{HUB_API}/seeder/version"))
            .send()
            .await
            .context("GET /seeder/version")?
            .error_for_status()
            .context("GET /seeder/version status")?
            .json::<VersionResponse>()
            .await
            .context("GET /seeder/version parse")?;
        Ok(resp.version)
    }

    /// Download the installer for the current arch. Returns raw bytes.
    /// Platform is "x64" or "x86", determined at compile time.
    pub async fn download_update(&self) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(format!("{HUB_API}/download/{APP_ARCH}"))
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .context("GET /download/{APP_ARCH}")?
            .error_for_status()
            .context("GET /download/{APP_ARCH} status")?
            .bytes()
            .await
            .context("GET /download/{APP_ARCH} read")?;
        Ok(resp.to_vec())
    }

    /// Network reachability check used by the seeder before starting.
    pub async fn ping(&self) -> bool {
        self.client
            .get(format!("{HUB_API}/servers"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Checks /api/v1/health → {status:"ok"} — shown in the status bar.
    pub async fn health_check(&self) -> bool {
        #[derive(Deserialize)]
        struct Resp { status: String }
        match self.client
            .get("https://api.hub.pstnsquad.ru/api/v1/health")
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) => r.json::<Resp>().await.map(|h| h.status == "ok").unwrap_or(false),
            Err(_) => false,
        }
    }
}
