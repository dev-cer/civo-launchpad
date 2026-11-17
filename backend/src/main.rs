//! Civo K8s Launchpad — Backend API
//!
//! Endpoint:
//!   GET  /healthz                          -> liveness/readiness probe
//!   POST /api/register                     -> daftar peserta + langsung dapat akun lab (API key)
//!   GET  /api/get-account?participant=ID   -> (legacy) assign sticky dummy API key tanpa registrasi
//!   POST /api/provision                    -> simulasi provisioning klaster
//!   GET  /api/status                       -> ringkasan pool (fasilitator)
//!   GET  /api/window                       -> jendela event {state: before|open|closed, open, close, now}
//!   GET  /api/registrations                -> export CSV peserta (Authorization: Bearer $ADMIN_TOKEN)
//!   POST /api/race/check · GET /api/race · POST /api/admin/race  -> race misi (lihat race.rs)
//!   GET  /api/leaderboard                  -> papan skor Flash Sale 12.12 (urut errors, lalu p95)
//!   POST /api/leaderboard                  -> kirim skor peserta (individu); dengan Bearer $ADMIN_TOKEN = hasil resmi fasilitator
//!
//! Env:
//!   WORKSHOP_ACCOUNTS   "id:apikey[:cluster],..." (dari Secret). id = KODE AKSES di kartu peserta;
//!                       cluster = nama cluster yang sudah disiapkan (pre-provisioned) untuk kode itu.
//!   OWN_ACCOUNTS=1          peserta memakai akun Civo sendiri (tanpa pool API key)
//!   CLUSTER_POOL=1          26 Nov: cluster disiapkan panitia. Peserta WAJIB register dengan kode akses
//!                           dari kartu check-in + email akun Civo miliknya (pre-work), lalu menerima
//!                           kubeconfig cluster-nya. Tidak ada API key yang dibagikan.
//!   SEATS                   "kode:cluster,..." (dari Secret workshop-seats, dibuat provision-workshop-pool)
//!   KUBECONFIG_DIR          folder kubeconfig <cluster>.yaml (Secret workshop-kubeconfigs, default /kubeconfigs)
//!   EVENT_OPEN / EVENT_CLOSE  jendela event RFC3339, mis. 2026-11-26T10:00:00+07:00 / 2026-11-26T23:59:59+07:00 (kosong = selalu buka)
//!   ADMIN_TOKEN         token export peserta    (dari Secret; kosong = endpoint export mati)
//!   REGISTRATIONS_FILE  path JSONL, mis. /data/registrations.jsonl (PVC) — dipakai untuk
//!                       memulihkan assignment setelah restart.
//! State akun in-memory (dipulihkan dari JSONL) -> backend WAJIB 1 replica.

mod race;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, services::ServeDir};

#[derive(Clone, Serialize, Deserialize, Debug)]
struct DummyAccount {
    id: String,
    api_key: String,
    #[serde(default)]
    cluster: String,
    assigned_to: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Score {
    name: String,
    #[serde(default, skip_serializing)]
    code: String,
    p95_ms: u32,
    errors: u32,
    rps: u32,
    official: bool,
    at: u64,
}

#[derive(Deserialize)]
struct ScoreRequest {
    #[serde(alias = "team")]
    name: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    p95_ms: u32,
    #[serde(default)]
    errors: u32,
    #[serde(default)]
    rps: u32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Registration {
    participant: String,
    name: String,
    email: String,
    company: String,
    role: String,
    event: String,
    experience: String,
    interests: Vec<String>,
    #[serde(default)]
    hero: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    timeline: String,
    #[serde(default)]
    early_access: bool,
    #[serde(default)]
    wants_call: bool,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    k8s_usage: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    civo_account: String,
    /// Token rahasia peserta (uuid), dipakai untuk connect cluster & cek race.
    #[serde(default)]
    token: String,
    account_id: String,
    seat: usize,
    created_at: u64,
}

struct AppState {
    accounts: Mutex<Vec<DummyAccount>>,
    regs: Mutex<Vec<Registration>>,
    reg_file: Option<String>,
    admin_token: Option<String>,
    scores: Mutex<Vec<Score>>,
    /// Jendela akses event (unix detik). None = selalu buka (edisi Academy).
    window: Option<(u64, u64)>,
    race: Mutex<race::RaceData>,
    race_file: String,
    last_check: Mutex<std::collections::HashMap<String, u64>>,
}

/// Parse "YYYY-MM-DDTHH:MM[:SS]+HH:MM" (RFC3339 sederhana) ke unix detik, tanpa dependency tambahan.
fn parse_ts(v: &str) -> Option<u64> {
    let v = v.trim();
    let (dt, off) = if let Some(i) = v.rfind(|c| c == '+' || c == '-').filter(|&i| i > 10) { (&v[..i], &v[i..]) }
                    else if let Some(x) = v.strip_suffix('Z') { (x, "+00:00") } else { return None };
    let (d, t) = dt.split_once('T')?;
    let dp: Vec<i64> = d.split('-').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let tp: Vec<i64> = t.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let (y, m, day) = (dp[0], dp[1], dp[2]);
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + day - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let sign = if off.starts_with('-') { -1 } else { 1 };
    let (oh, om) = off[1..].split_once(':')?;
    let secs = days * 86400 + tp[0] * 3600 + tp.get(1).copied().unwrap_or(0) * 60 + tp.get(2).copied().unwrap_or(0)
        - sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60);
    u64::try_from(secs).ok()
}

fn window_state(w: Option<(u64, u64)>) -> &'static str {
    match w { None => "open", Some((o, c)) => { let n = now(); if n < o { "before" } else if n > c { "closed" } else { "open" } } }
}

fn is_admin(state: &AppState, headers: &HeaderMap) -> bool {
    match &state.admin_token {
        Some(t) => headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(format!("Bearer {t}").as_str()),
        None => false,
    }
}

async fn get_window(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (open, close) = state.window.map(|(o, c)| (Some(o), Some(c))).unwrap_or((None, None));
    Json(serde_json::json!({ "state": window_state(state.window), "open": open, "close": close, "now": now() }))
}

#[derive(Deserialize)]
struct AccountQuery {
    participant: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ProvisionRequest {
    api_key: String,
    cluster_name: String,
}

#[derive(Deserialize)]
struct RegisterRequest {
    participant: String,
    name: String,
    email: String,
    #[serde(default)]
    company: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    experience: String,
    #[serde(default)]
    interests: Vec<String>,
    #[serde(default)]
    consent: bool,
    #[serde(default)]
    hero: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    timeline: String,
    #[serde(default)]
    early_access: bool,
    #[serde(default)]
    wants_call: bool,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    k8s_usage: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    access_code: String,
    #[serde(default)]
    civo_account: String,
    #[serde(default)]
    civo_signup: bool,
}

fn pool_mode() -> bool {
    env::var("CLUSTER_POOL").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

/// SEATS="kode:cluster,..." -> kursi dengan cluster yang sudah disiapkan panitia (tanpa API key).
fn load_seats() -> Vec<DummyAccount> {
    env::var("SEATS").unwrap_or_default().split(',')
        .filter_map(|pair| {
            let (code, cluster) = pair.trim().split_once(':')?;
            let (code, cluster) = (code.trim(), cluster.trim());
            if code.is_empty() || cluster.is_empty() { return None; }
            Some(DummyAccount { id: code.to_string(), api_key: String::new(), cluster: cluster.to_string(), assigned_to: None })
        })
        .collect()
}

fn read_kubeconfig(cluster: &str) -> Option<String> {
    if cluster.is_empty() || !cluster.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { return None; }
    let dir = env::var("KUBECONFIG_DIR").unwrap_or_else(|_| "/kubeconfigs".to_string());
    std::fs::read_to_string(format!("{dir}/{cluster}.yaml")).ok()
}

fn load_accounts() -> Vec<DummyAccount> {
    if pool_mode() { return load_seats(); }
    let raw = env::var("WORKSHOP_ACCOUNTS").unwrap_or_else(|_| {
        "user-01:civo-apikey-x9q2m,user-02:civo-apikey-r4v8p,user-03:civo-apikey-k7b1z".to_string()
    });
    raw.split(',')
        .filter_map(|pair| {
            let mut it = pair.trim().splitn(3, ':');
            let id = it.next()?.trim().to_string();
            let key = it.next()?.trim().to_string();
            let cluster = it.next().unwrap_or("").trim().to_string();
            if id.is_empty() || key.is_empty() { return None; }
            Some(DummyAccount { id, api_key: key, cluster, assigned_to: None })
        })
        .collect()
}

fn load_registrations(path: &Option<String>) -> Vec<Registration> {
    let Some(p) = path else { return vec![] };
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    let all: Vec<Registration> = BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect();
    // Baris terakhir per email menang (registrasi bisa diperbarui, mis. pindah ke kursi cadangan).
    let mut out: Vec<Registration> = vec![];
    for r in all { if let Some(i) = out.iter().position(|x| x.email == r.email) { out[i] = r } else { out.push(r) } }
    out
}

fn append_reg(path: &Option<String>, r: &Registration) {
    if let Some(p) = path {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{}", serde_json::to_string(r).unwrap_or_default());
        }
    }
}

fn clean(s: &str, max: usize) -> String {
    s.trim().chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Lead score untuk triase SQL (bukan pengganti kualifikasi sales). Maks 10; kandidat SQL >= 6.
/// Field mengikuti Google Form webinar: job title (bebas), status Kubernetes, email bisnis.
fn lead_score(r: &Registration) -> u32 {
    let t = r.role.to_lowercase();
    let senior = ["cto", "cio", "chief", "head", "director", "vp", "manager", "lead", "architect", "owner", "founder"];
    let practitioner = ["devops", "sre", "platform", "cloud", "infra", "engineer", "developer", "admin"];
    let mut s = if senior.iter().any(|k| t.contains(k)) { 3 } else if practitioner.iter().any(|k| t.contains(k)) { 2 } else { 0 };
    s += match r.k8s_usage.as_str() {
        "Yes" => 3,
        "Not yet, but interested in adopting Kubernetes" => 3,
        "Not sure" => 1,
        _ => 0,
    };
    let free = ["gmail.com", "yahoo.com", "yahoo.co.id", "hotmail.com", "outlook.com", "icloud.com", "ymail.com"];
    if !free.iter().any(|d| r.email.ends_with(&format!("@{d}"))) { s += 2; }
    if !r.company.is_empty() { s += 1; }
    if r.early_access { s += 1; }
    if r.wants_call { s += 2; }
    s.min(10)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let mut accounts = load_accounts();
    let reg_file = env::var("REGISTRATIONS_FILE").ok().filter(|s| !s.is_empty());
    let regs = load_registrations(&reg_file);
    // Pulihkan assignment akun dari registrasi yang tersimpan.
    for r in &regs {
        if let Some(a) = accounts.iter_mut().find(|a| a.id == r.account_id) {
            a.assigned_to = Some(r.email.clone());
        }
    }
    println!("Loaded {} accounts, {} registrations", accounts.len(), regs.len());

    let state = Arc::new(AppState {
        accounts: Mutex::new(accounts),
        regs: Mutex::new(regs),
        reg_file,
        admin_token: env::var("ADMIN_TOKEN").ok().filter(|s| !s.is_empty()),
        scores: Mutex::new(vec![]),
        race: Mutex::new({
            let f = env::var("RACE_FILE").unwrap_or_else(|_| "/data/race.json".to_string());
            let mut d = race::load(&f);
            if d.start.is_none() { d.start = env::var("RACE_START").ok().and_then(|v| parse_ts(&v)); }
            d
        }),
        race_file: env::var("RACE_FILE").unwrap_or_else(|_| "/data/race.json".to_string()),
        last_check: Mutex::new(std::collections::HashMap::new()),
        window: match (env::var("EVENT_OPEN").ok().and_then(|v| parse_ts(&v)), env::var("EVENT_CLOSE").ok().and_then(|v| parse_ts(&v))) {
            (Some(o), Some(c)) => Some((o, c)),
            _ => None,
        },
    });
    if let Some((o, c)) = state.window { println!("Event window: {o} .. {c} (unix)"); }

    // Hapus kubeconfig cluster peserta otomatis setelah event ditutup (23:59 WIB).
    if let Some((_, close)) = state.window {
        tokio::spawn(async move {
            loop {
                if now() > close { let n = purge_own_kubeconfigs(); if n > 0 { println!("Purged {n} participant kubeconfigs after event close"); } }
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            }
        });
    }
    let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "static".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/register", post(register))
        .route("/api/get-account", get(get_dummy_account))
        .route("/api/provision", post(provision_cluster))
        .route("/api/status", get(pool_status))
        .route("/api/window", get(get_window))
        .route("/api/registrations", get(export_registrations))
        .route("/api/leaderboard", get(get_leaderboard).post(post_score))
        .route("/api/race", get(get_race))
        .route("/api/race/check", post(race_check))
        .route("/api/admin/race", post(admin_race))
        .route("/api/cluster/connect", post(connect_cluster))
        .fallback_service(ServeDir::new(static_dir))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    println!("🚀 Workshop server listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[derive(Deserialize)]
struct RaceCheckRequest { email: String, #[serde(default)] access_code: String, #[serde(default)] token: String, mission: String }
#[derive(Deserialize)]
struct RaceAdminRequest { action: String }

async fn get_race(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let d = state.race.lock().await;
    let t = now();
    let status = match (d.start, d.stop) { (_, Some(_)) => "finished", (Some(s), None) if t >= s => "running", (Some(_), None) => "scheduled", _ => "waiting" };
    Json(serde_json::json!({ "status": "success", "race": status, "start": d.start, "stop": d.stop, "now": t,
        "missions": race::MISSIONS, "players": race::board(&d) }))
}

async fn admin_race(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<RaceAdminRequest>) -> impl IntoResponse {
    if !is_admin(&state, &headers) { return err(StatusCode::UNAUTHORIZED, "Admin token required."); }
    let mut d = state.race.lock().await;
    match req.action.as_str() {
        "start" => { d.start = Some(now()); d.stop = None; }
        "stop" => { d.stop = Some(now()); }
        "reset" => { *d = race::RaceData::default(); }
        "purge" => { let n = purge_own_kubeconfigs(); return (StatusCode::OK, Json(serde_json::json!({ "status": "success", "purged": n }))); }
        _ => return err(StatusCode::BAD_REQUEST, "action must be start, stop, reset or purge"),
    }
    race::save(&state.race_file, &d);
    (StatusCode::OK, Json(serde_json::json!({ "status": "success", "start": d.start, "stop": d.stop })))
}

async fn race_check(State(state): State<Arc<AppState>>, Json(req): Json<RaceCheckRequest>) -> impl IntoResponse {
    let email = clean(&req.email, 120).to_lowercase();
    let code = clean(&req.access_code, 40).to_lowercase();
    let mission = clean(&req.mission, 10);
    let Some(midx) = race::MISSIONS.iter().position(|m| m.eq_ignore_ascii_case(&mission)) else {
        return err(StatusCode::BAD_REQUEST, "Unknown mission.");
    };
    let mission = race::MISSIONS[midx];
    // Peserta harus terdaftar, dan kode akses harus cocok dengan kursinya.
    let (name, cluster) = {
        let regs = state.regs.lock().await;
        let Some(r) = regs.iter().find(|r| r.email == email) else { return err(StatusCode::FORBIDDEN, "Register in Launchpad first."); };
        let tok_ok = !r.token.is_empty() && r.token == clean(&req.token, 64);
        if !tok_ok && (r.account_id == "own" || r.account_id.to_lowercase() != code) { return err(StatusCode::FORBIDDEN, "Sign in to Launchpad again with your registration details."); }
        if r.account_id == "own" { (r.name.clone(), format!("own:{}", r.token)) } else {
            let accs = state.accounts.lock().await;
            (r.name.clone(), accs.iter().find(|a| a.id == r.account_id).map(|a| a.cluster.clone()).unwrap_or_default())
        }
    };
    {
        let d = state.race.lock().await;
        let t = now();
        match (d.start, d.stop) {
            (_, Some(_)) => return err(StatusCode::CONFLICT, "The race is over — see the leaderboard."),
            (Some(s), None) if t >= s => {}
            _ => return err(StatusCode::CONFLICT, "The race has not started yet. Wait for the facilitator's signal."),
        }
        let done: Vec<String> = d.done.get(&email).map(|(_, v)| v.iter().map(|x| x.0.clone()).collect()).unwrap_or_default();
        if let Some(prev) = race::MISSIONS[..midx].iter().find(|m| !done.iter().any(|x| x == *m)) {
            return err(StatusCode::CONFLICT, &format!("Finish {prev} first — missions count in order."));
        }
        if done.iter().any(|x| x == mission) {
            return (StatusCode::OK, Json(serde_json::json!({ "status": "success", "passed": true, "already": true, "mission": mission, "players": race::board(&d) })));
        }
    }
    {
        let mut lc = state.last_check.lock().await;
        let t = now();
        let cool: u64 = env::var("RACE_COOLDOWN").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        if let Some(prev) = lc.get(&email) { if t < prev + cool { return err(StatusCode::TOO_MANY_REQUESTS, "Checking too often — wait a few seconds."); } }
        lc.insert(email.clone(), t);
    }
    let kc = match cluster.strip_prefix("own:") {
        Some(tok) => own_kc_path(tok),
        None => format!("{}/{}.yaml", env::var("KUBECONFIG_DIR").unwrap_or_else(|_| "/kubeconfigs".to_string()), cluster),
    };
    if cluster.is_empty() || !std::path::Path::new(&kc).exists() {
        return err(StatusCode::PRECONDITION_FAILED, if cluster.starts_with("own:") { "Connect your cluster first: upload its kubeconfig in Launchpad (M0)." } else { "Your cluster is not attached to Launchpad — ask a facilitator." });
    }
    let checks = race::verify(&kc, mission).await;
    let passed = checks.iter().all(|c| c.ok);
    let mut d = state.race.lock().await;
    let mut at = None;
    if passed && d.stop.is_none() {
        let t = now();
        let e = d.done.entry(email.clone()).or_insert((name.clone(), vec![]));
        if !e.1.iter().any(|x| x.0 == mission) { e.1.push((mission.to_string(), t)); }
        at = Some(t.saturating_sub(d.start.unwrap_or(t)));
        race::save(&state.race_file, &d);
    }
    (StatusCode::OK, Json(serde_json::json!({ "status": "success", "passed": passed, "mission": mission, "elapsed": at, "checks": checks, "players": race::board(&d) })))
}

fn own_kc_dir() -> String { env::var("OWN_KUBECONFIG_DIR").unwrap_or_else(|_| "/data/own-kubeconfigs".to_string()) }
fn own_kc_path(token: &str) -> String {
    let t: String = token.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    format!("{}/{t}.yaml", own_kc_dir())
}
fn purge_own_kubeconfigs() -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(own_kc_dir()) { for e in rd.flatten() { if std::fs::remove_file(e.path()).is_ok() { n += 1 } } }
    n
}

/// Tolak kubeconfig yang bisa membuat server menjalankan perintah, membaca file lokal,
/// atau menghubungi alamat internal (SSRF). Hanya data tertanam (*-data) dan token yang diterima.
fn validate_kubeconfig(kc: &str) -> Result<(), String> {
    if kc.len() > 64 * 1024 { return Err("The kubeconfig is too large.".into()) }
    if !kc.contains("apiVersion") || !kc.contains("clusters") { return Err("This does not look like a kubeconfig file.".into()) }
    for raw in kc.lines() {
        let l = raw.trim_start().trim_start_matches("- ").trim();
        let key = l.split(':').next().unwrap_or("").trim().trim_matches('"');
        let banned = ["exec", "auth-provider", "tokenFile", "client-certificate", "client-key", "certificate-authority", "proxy-url", "command", "username", "password", "impersonate", "as", "as-groups", "as-uid"];
        if banned.contains(&key) {
            return Err(format!("This kubeconfig uses '{key}', which Launchpad does not accept. Use the file from: civo kubernetes config <cluster> --region JKT1"));
        }
        if key == "server" {
            let url = l.splitn(2, ':').nth(1).unwrap_or("").trim().trim_matches('"').to_lowercase();
            let host = url.strip_prefix("https://").ok_or("The API server must use https://")?.split(['/', ':']).next().unwrap_or("").to_string();
            let private = host.is_empty() || host == "localhost" || host.ends_with(".local") || host.ends_with(".svc") || host.ends_with(".internal") || host.contains("cluster.local")
                || host.starts_with("10.") || host.starts_with("127.") || host.starts_with("192.168.") || host.starts_with("169.254.") || host.starts_with("0.") || host.starts_with('[')
                || (host.starts_with("172.") && host.split('.').nth(1).and_then(|x| x.parse::<u8>().ok()).map(|x| (16..=31).contains(&x)).unwrap_or(false))
                || (host.starts_with("100.") && host.split('.').nth(1).and_then(|x| x.parse::<u8>().ok()).map(|x| (64..=127).contains(&x)).unwrap_or(false));
            if private { return Err("The API server address must be your Civo cluster's public endpoint.".into()) }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct ConnectRequest { email: String, token: String, kubeconfig: String, #[serde(default)] consent: bool }

/// Peserta menghubungkan cluster di akun Civo-nya sendiri dengan mengunggah kubeconfig.
async fn connect_cluster(State(state): State<Arc<AppState>>, Json(req): Json<ConnectRequest>) -> impl IntoResponse {
    if !req.consent { return err(StatusCode::BAD_REQUEST, "Please accept that Launchpad reads your cluster until 23:59 WIB today."); }
    let email = clean(&req.email, 120).to_lowercase();
    let tok = clean(&req.token, 64);
    {
        let regs = state.regs.lock().await;
        match regs.iter().find(|r| r.email == email) {
            Some(r) if !r.token.is_empty() && r.token == tok => {
                if r.account_id != "own" { return err(StatusCode::CONFLICT, "You are on a backup seat prepared by the team — no upload needed."); }
            }
            _ => return err(StatusCode::FORBIDDEN, "Sign in to Launchpad again with your registration details."),
        }
    }
    if let Err(e) = validate_kubeconfig(&req.kubeconfig) { return err(StatusCode::BAD_REQUEST, &e); }
    let _ = std::fs::create_dir_all(own_kc_dir());
    let path = own_kc_path(&tok);
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, &req.kubeconfig).is_err() { return err(StatusCode::INTERNAL_SERVER_ERROR, "Could not store the kubeconfig."); }
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)); }
    let checks = race::verify(&tmp, "M0").await;
    if !checks.iter().all(|c| c.ok) {
        let _ = std::fs::remove_file(&tmp);
        let why = checks.iter().find(|c| !c.ok).map(|c| format!("{}: {}", c.name, c.hint)).unwrap_or_default();
        return err(StatusCode::BAD_REQUEST, &format!("Launchpad could not use this cluster yet. {why}"));
    }
    let _ = std::fs::rename(&tmp, &path);
    (StatusCode::OK, Json(serde_json::json!({ "status": "success", "connected": true, "checks": checks })))
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (code, Json(serde_json::json!({ "status": "error", "message": msg })))
}

async fn register(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<RegisterRequest>) -> impl IntoResponse {
    // Academy (/academy/) selalu buka dan tidak memakai kursi/kode workshop: hanya mencatat lead.
    let academy = req.event.to_lowercase().contains("academy");
    match if academy { "open" } else { window_state(state.window) } {
        "before" if !is_admin(&state, &headers) => return err(StatusCode::FORBIDDEN, "Launchpad is not open yet — it opens when the event starts."),
        "closed" if !is_admin(&state, &headers) => return err(StatusCode::FORBIDDEN, "This Launchpad closed at 23:59 WIB on event day."),
        _ => {}
    }
    let name = clean(&req.name, 80);
    let email = clean(&req.email, 120).to_lowercase();
    if name.chars().count() < 2 {
        return err(StatusCode::BAD_REQUEST, "Name must be at least 2 characters.");
    }
    let valid_email = email.split_once('@').map(|(u, d)| !u.is_empty() && d.contains('.')).unwrap_or(false);
    if !valid_email {
        return err(StatusCode::BAD_REQUEST, "Invalid email address.");
    }
    if !req.consent {
        return err(StatusCode::BAD_REQUEST, "Data consent is required.");
    }

    let mut accounts = state.accounts.lock().await;
    let mut regs = state.regs.lock().await;

    // Email yang sama (mis. ganti browser) mendapat akun yang sama.
    let code = clean(&req.access_code, 40).to_lowercase();
    if academy {
        if let Some(r) = regs.iter().find(|r| r.email == email) {
            // Sudah terdaftar (mis. peserta workshop): jangan buat registrasi baru, cukup sambut kembali.
            return (StatusCode::OK, Json(serde_json::json!({ "status": "success", "returning": true, "api_key": null, "cluster": "", "own": true, "account_id": "own", "seat": r.seat, "name": r.name })));
        }
    }
    if let Some(r) = regs.iter().find(|r| r.email == email) {
        let acc = accounts.iter().find(|a| a.id == r.account_id);
        if pool_mode() {
            let civo = clean(&req.civo_account, 120).to_lowercase();
            let idx = regs.iter().position(|x| x.email == email).unwrap();
            if regs[idx].account_id == "own" {
                // Peserta dengan cluster sendiri: buktikan pemilik dengan akun Civo yang sama.
                if !regs[idx].civo_account.is_empty() && regs[idx].civo_account != civo {
                    return err(StatusCode::FORBIDDEN, "This email is already registered with a different Civo account.");
                }
                // Kursi cadangan dari fasilitator: kode valid mengubah registrasi ke cluster panitia.
                if !code.is_empty() {
                    let Some(a) = accounts.iter_mut().find(|a| a.id.to_lowercase() == code) else { return err(StatusCode::NOT_FOUND, "Unknown access code. Check your card or ask a facilitator.") };
                    if a.assigned_to.is_some() { return err(StatusCode::CONFLICT, "This access code is already in use. Please ask a facilitator."); }
                    a.assigned_to = Some(email.clone());
                    regs[idx].account_id = a.id.clone();
                }
            } else if regs[idx].account_id.to_lowercase() != code {
                return err(StatusCode::FORBIDDEN, "This email is already registered. Enter the same access code from your check-in card.");
            }
            if regs[idx].token.is_empty() { regs[idx].token = uuid::Uuid::new_v4().to_string(); }
            append_reg(&state.reg_file, &regs[idx]);
            let r = &regs[idx];
            let cluster = accounts.iter().find(|a| a.id == r.account_id).map(|a| a.cluster.clone()).unwrap_or_default();
            return (StatusCode::OK, Json(serde_json::json!({
                "status": "success", "returning": true, "api_key": null, "cluster": cluster, "own": r.account_id == "own",
                "kubeconfig": read_kubeconfig(&cluster), "account_id": r.account_id, "seat": r.seat, "name": r.name, "token": r.token
            })));
        }
        return (StatusCode::OK, Json(serde_json::json!({
            "status": "success", "returning": true, "api_key": acc.map(|a| a.api_key.clone()),
            "cluster": acc.map(|a| a.cluster.clone()).unwrap_or_default(),
            "account_id": r.account_id, "seat": r.seat, "name": r.name
        })));
    }
    // OWN_ACCOUNTS=1 (workshop "Beyond the Container"): peserta memakai akun Civo sendiri,
    // backend hanya mencatat registrasi (lead) tanpa membagikan API key dari pool.
    let own_accounts = env::var("OWN_ACCOUNTS").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    if pool_mode() && !academy && (!req.civo_signup || clean(&req.civo_account, 120).chars().count() < 3) {
        return err(StatusCode::BAD_REQUEST, "Sign up for your own Civo account first (pre-work), then enter the email or username of that account.");
    }
    // 26 Nov: tanpa kode = peserta membuat cluster di akun Civo sendiri; kode = kursi cadangan panitia.
    if own_accounts || academy || (pool_mode() && code.is_empty()) {
        let reg = Registration {
            participant: clean(&req.participant, 64), name: name.clone(), email,
            company: clean(&req.company, 100), role: clean(&req.role, 60), event: clean(&req.event, 80),
            experience: clean(&req.experience, 40), interests: req.interests.iter().take(8).map(|s| clean(s, 40)).collect(),
            hero: clean(&req.hero, 20), platform: clean(&req.platform, 60), timeline: clean(&req.timeline, 30),
            early_access: req.early_access, wants_call: req.wants_call, phone: clean(&req.phone, 24),
            location: clean(&req.location, 60), k8s_usage: clean(&req.k8s_usage, 60), source: clean(&req.source, 120),
            civo_account: clean(&req.civo_account, 120).to_lowercase(), token: uuid::Uuid::new_v4().to_string(), account_id: "own".to_string(), seat: regs.len() + 1, created_at: now(),
        };
        if let Some(p) = &state.reg_file {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{}", serde_json::to_string(&reg).unwrap_or_default());
            }
        }
        let body = serde_json::json!({ "status": "success", "returning": false, "api_key": null, "cluster": "", "own": true,
            "account_id": "own", "seat": reg.seat, "name": reg.name, "token": reg.token });
        regs.push(reg);
        return (StatusCode::CREATED, Json(body));
    }
    let civo_account = clean(&req.civo_account, 120).to_lowercase();
    if pool_mode() {
        if !req.civo_signup || civo_account.chars().count() < 3 {
            return err(StatusCode::BAD_REQUEST, "Sign up for your own Civo account first (pre-work), then enter the email or username of that account.");
        }
    }
    // Kode akses dari kartu check-in (26 Nov) -> akun + cluster yang sudah disiapkan untuk kode itu.
    let acc = if !code.is_empty() {
        match accounts.iter_mut().find(|a| a.id.to_lowercase() == code) {
            Some(a) if a.assigned_to.is_none() => a,
            Some(_) => return err(StatusCode::CONFLICT, "This access code is already in use. Please ask a facilitator."),
            None => return err(StatusCode::NOT_FOUND, "Unknown access code. Check your card or ask a facilitator."),
        }
    } else {
        match accounts.iter_mut().find(|a| a.assigned_to.is_none()) {
            Some(a) => a,
            None => return err(StatusCode::SERVICE_UNAVAILABLE, "All lab seats are taken. Please contact a facilitator."),
        }
    };
    acc.assigned_to = Some(email.clone());
    let reg = Registration {
        participant: clean(&req.participant, 64),
        name: name.clone(),
        email,
        company: clean(&req.company, 100),
        role: clean(&req.role, 60),
        event: clean(&req.event, 80),
        experience: clean(&req.experience, 40),
        interests: req.interests.iter().take(8).map(|s| clean(s, 40)).collect(),
        hero: clean(&req.hero, 20),
        platform: clean(&req.platform, 60),
        timeline: clean(&req.timeline, 30),
        early_access: req.early_access,
        wants_call: req.wants_call,
        phone: clean(&req.phone, 24),
        location: clean(&req.location, 60),
        k8s_usage: clean(&req.k8s_usage, 60),
        source: clean(&req.source, 120),
        civo_account,
        token: uuid::Uuid::new_v4().to_string(),
        account_id: acc.id.clone(),
        seat: regs.len() + 1,
        created_at: now(),
    };
    if let Some(p) = &state.reg_file {
        match OpenOptions::new().create(true).append(true).open(p) {
            Ok(mut f) => { let _ = writeln!(f, "{}", serde_json::to_string(&reg).unwrap_or_default()); }
            Err(e) => eprintln!("WARN: gagal menulis {p}: {e}"),
        }
    }
    let pool = pool_mode();
    let body = serde_json::json!({
        "status": "success", "returning": false,
        "api_key": if pool { serde_json::Value::Null } else { serde_json::Value::String(acc.api_key.clone()) },
        "cluster": acc.cluster, "kubeconfig": if pool { read_kubeconfig(&acc.cluster) } else { None },
        "account_id": acc.id, "seat": reg.seat, "name": reg.name, "token": reg.token, "own": false
    });
    regs.push(reg);
    (StatusCode::CREATED, Json(body))
}

async fn get_dummy_account(State(state): State<Arc<AppState>>, Query(q): Query<AccountQuery>) -> impl IntoResponse {
    if pool_mode() { return err(StatusCode::NOT_FOUND, "Not available in cluster-pool mode."); }
    let mut accounts = state.accounts.lock().await;
    let participant = q.participant.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if let Some(acc) = accounts.iter().find(|a| a.assigned_to.as_deref() == Some(participant.as_str())) {
        return (StatusCode::OK, Json(serde_json::json!({ "status": "success", "api_key": acc.api_key, "account_id": acc.id, "participant": participant })));
    }
    if let Some(acc) = accounts.iter_mut().find(|a| a.assigned_to.is_none()) {
        acc.assigned_to = Some(participant.clone());
        return (StatusCode::OK, Json(serde_json::json!({ "status": "success", "api_key": acc.api_key, "account_id": acc.id, "participant": participant })));
    }
    err(StatusCode::SERVICE_UNAVAILABLE, "No accounts left.")
}

async fn provision_cluster(Json(payload): Json<ProvisionRequest>) -> impl IntoResponse {
    (StatusCode::ACCEPTED, Json(serde_json::json!({ "status": "building", "cluster_name": payload.cluster_name })))
}

async fn pool_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let accounts = state.accounts.lock().await;
    let regs = state.regs.lock().await;
    let used = accounts.iter().filter(|a| a.assigned_to.is_some()).count();
    let sql = regs.iter().filter(|r| lead_score(r) >= 6).count();
    let early = regs.iter().filter(|r| r.early_access).count();
    Json(serde_json::json!({ "total": accounts.len(), "used": used, "free": accounts.len() - used, "registered": regs.len(), "sql_candidates": sql, "early_access": early }))
}

fn csv(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

async fn export_registrations(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let Some(token) = &state.admin_token else {
        return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], "not found".to_string());
    };
    let ok = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(format!("Bearer {token}").as_str());
    if !ok {
        return (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "text/plain")], "unauthorized".to_string());
    }
    let regs = state.regs.lock().await;
    let mut out = String::from("seat,created_at,lead_score,sql_candidate,name,email,whatsapp,company,job_title,location,k8s_usage,event,source,civo_account,account_id\n");
    for r in regs.iter() {
        let sc = lead_score(r);
        out.push_str(&format!("{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n", r.seat, r.created_at, sc, sc >= 6, csv(&r.name), csv(&r.email),
            csv(&r.phone), csv(&r.company), csv(&r.role), csv(&r.location), csv(&r.k8s_usage), csv(&r.event), csv(&r.source), csv(&r.civo_account), csv(&r.account_id)));
    }
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/csv; charset=utf-8")], out)
}

fn ranked(scores: &[Score]) -> Vec<Score> {
    // Satu baris per peserta (kunci: access code, atau nama jika tanpa code).
    // Hasil resmi fasilitator menang atas self-test; jika sama-sama, yang terbaru.
    let key = |s: &Score| if s.code.is_empty() { s.name.to_lowercase() } else { s.code.to_uppercase() };
    let mut best: Vec<Score> = vec![];
    for sc in scores {
        match best.iter_mut().find(|b| key(b) == key(sc)) {
            Some(b) => { if sc.official || !b.official { *b = sc.clone(); } }
            None => best.push(sc.clone()),
        }
    }
    best.sort_by(|a, b| a.errors.cmp(&b.errors).then(a.p95_ms.cmp(&b.p95_ms)).then(b.rps.cmp(&a.rps)));
    best
}

async fn get_leaderboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let scores = state.scores.lock().await;
    Json(serde_json::json!({ "status": "success", "players": ranked(&scores) }))
}

async fn post_score(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<ScoreRequest>) -> impl IntoResponse {
    let name = clean(&req.name, 40);
    let code = clean(&req.code, 40).to_uppercase();
    if name.chars().count() < 2 {
        return err(StatusCode::BAD_REQUEST, "Name must be at least 2 characters.");
    }
    let official = is_admin(&state, &headers);
    if !official && window_state(state.window) != "open" {
        return err(StatusCode::FORBIDDEN, "The leaderboard only accepts scores while the event is open.");
    }
    let mut scores = state.scores.lock().await;
    if scores.len() >= 2000 {
        return err(StatusCode::TOO_MANY_REQUESTS, "Leaderboard is full.");
    }
    scores.push(Score { name, code, p95_ms: req.p95_ms.min(600_000), errors: req.errors, rps: req.rps.min(1_000_000), official, at: now() });
    let players = ranked(&scores);
    (StatusCode::CREATED, Json(serde_json::json!({ "status": "success", "official": official, "players": players })))
}
