//! Integrasi Certifier (certifier.io): sertifikat + badge resmi dikirim Certifier ke e-mail peserta,
//! lengkap dengan halaman verifikasi, QR code, dan tombol Add to LinkedIn milik Certifier.
//!
//! Launchpad hanya menjadi pemicu: siapa berhak apa (race::certificate) → POST create-issue-send.
//!   Env (dari Secret launchpad-certifier, opsional — tanpa ini Launchpad memakai sertifikat bawaan /cert/<ID>):
//!     CERTIFIER_TOKEN                 access token (Certifier → Settings → Developers)
//!     CERTIFIER_GROUP_COMPLETION      ID Credential Template untuk 6 misi
//!     CERTIFIER_GROUP_PARTICIPATION   ID Credential Template untuk M0–M2
//!     CERTIFIER_API (opsional)        default https://api.certifier.io
//!     CERTIFIER_FILE (opsional)       default /data/certifier.json (catatan siapa sudah dikirimi apa)
//! Idempoten: satu kredensial per (email, jenis). Peserta yang naik dari Participation ke Completion
//! setelah race ditutup otomatis dikirimi Completion pada penerbitan berikutnya.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Config {
    pub api: String,
    token: String,
    pub group_completion: String,
    pub group_participation: String,
    pub file: String,
}

impl Config {
    pub fn from_env() -> Option<Config> {
        let v = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Some(Config {
            api: v("CERTIFIER_API").unwrap_or_else(|| "https://api.certifier.io".into()).trim_end_matches('/').to_string(),
            token: v("CERTIFIER_TOKEN")?,
            group_completion: v("CERTIFIER_GROUP_COMPLETION")?,
            group_participation: v("CERTIFIER_GROUP_PARTICIPATION")?,
            file: v("CERTIFIER_FILE").unwrap_or_else(|| "/data/certifier.json".into()),
        })
    }
    pub fn group(&self, kind: &str) -> &str { if kind == "Completion" { &self.group_completion } else { &self.group_participation } }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Issued { pub kind: String, pub credential_id: String, pub public_id: String, pub at: u64 }

/// email -> kredensial yang sudah dikirim (bisa dua: Participation lalu Completion).
pub type Ledger = HashMap<String, Vec<Issued>>;

pub fn load(path: &str) -> Ledger { std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
pub fn save(path: &str, l: &Ledger) { if let Ok(s) = serde_json::to_string_pretty(l) { let _ = std::fs::write(path, s); } }

pub fn has(l: &Ledger, email: &str, kind: &str) -> bool { l.get(email).map(|v| v.iter().any(|i| i.kind == kind)).unwrap_or(false) }

/// Satu panggilan create-issue-send. Certifier yang mengirim e-mail ke peserta.
pub async fn issue(client: &reqwest::Client, cfg: &Config, kind: &str, name: &str, email: &str) -> Result<Issued, String> {
    let body = serde_json::json!({
        "groupId": cfg.group(kind),
        "recipient": { "name": name, "email": email },
        "issueDate": "2026-11-26",
    });
    let r = client.post(format!("{}/v1/credentials/create-issue-send", cfg.api))
        .bearer_auth(&cfg.token)
        .header("Certifier-Version", "2022-10-26")
        .header("Accept", "application/json")
        .json(&body).send().await.map_err(|e| format!("Certifier unreachable: {e}"))?;
    let code = r.status();
    let text = r.text().await.unwrap_or_default();
    if !code.is_success() {
        let msg: String = text.chars().take(300).collect();
        return Err(format!("Certifier HTTP {}: {msg}", code.as_u16()));
    }
    let j: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    Ok(Issued {
        kind: kind.to_string(),
        credential_id: j["id"].as_str().unwrap_or_default().to_string(),
        public_id: j["publicId"].as_str().unwrap_or_default().to_string(),
        at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}
