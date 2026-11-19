//! arupacloud.com — company landing page (Askama + HTMX), served by the same binary as the workshop.
//!
//! Host routing: when the request Host is in SITE_HOSTS (e.g. "arupacloud.com,www.arupacloud.com"),
//!   /            -> Arupa Cloud landing page
//!   www.<apex>/* -> 301 to the apex
//! Everything else (/register, /workshop, /api/*, /cert/*, ...) is shared with the workshop server.
//!
//!   GET  /site/solutions/:cat   -> HTMX fragment: solution explorer panel
//!   POST /site/recommend        -> HTMX fragment: recommended stack for the ticked needs
//!   GET  /site/contact          -> HTMX fragment: consultation form (optional ?need=)
//!   POST /site/contact          -> HTMX fragment: form with errors, or thank-you card (stored in SQLite `leads`)
//!   GET  /admin/leads           -> admin table (HTTP Basic: admin / ADMIN_TOKEN) + /admin/leads.csv
//!
//! Content follows the Arupa Company Profile 2025 (external version). No prices on the site.

use askama::Template;
use axum::{
    body::Body,
    extract::{Form, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{env, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};

pub struct SiteState {
    hosts: Vec<String>,
    db: Option<Mutex<Connection>>,
    admin_token: Option<String>,
}

impl SiteState {
    pub fn from_env(admin_token: Option<String>) -> Arc<Self> {
        let hosts: Vec<String> = env::var("SITE_HOSTS").unwrap_or_default().split(',')
            .map(|h| h.trim().to_lowercase()).filter(|h| !h.is_empty()).collect();
        let path = env::var("SEATS_DB").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/data/register.db".to_string());
        let db = open(&path).map(Mutex::new).map_err(|e| eprintln!("Leads DB unavailable at {path}: {e}")).ok();
        if !hosts.is_empty() { println!("Site: arupacloud landing page on {}", hosts.join(", ")); }
        Arc::new(SiteState { hosts, db, admin_token })
    }
    fn seats(&self) -> i64 {
        self.db.as_ref().and_then(|d| d.lock().ok().and_then(|c| c.query_row("SELECT COUNT(*) FROM seats", [], |r| r.get(0)).ok())).unwrap_or(0)
    }
}

fn open(path: &str) -> rusqlite::Result<Connection> {
    if let Some(dir) = std::path::Path::new(path).parent() { let _ = std::fs::create_dir_all(dir); }
    let c = Connection::open(path)?;
    c.execute_batch(
        "PRAGMA busy_timeout=3000;
         CREATE TABLE IF NOT EXISTS seats (seat INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            name TEXT NOT NULL, email TEXT NOT NULL UNIQUE, phone TEXT NOT NULL DEFAULT '', company TEXT NOT NULL DEFAULT '',
            role TEXT NOT NULL DEFAULT '', experience TEXT NOT NULL DEFAULT '', civo_account TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL DEFAULT '', event TEXT NOT NULL DEFAULT '', consent INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE IF NOT EXISTS leads (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL,
            name TEXT NOT NULL, email TEXT NOT NULL, phone TEXT NOT NULL DEFAULT '', company TEXT NOT NULL DEFAULT '',
            need TEXT NOT NULL DEFAULT '', message TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT '', consent INTEGER NOT NULL DEFAULT 0);",
    )?;
    Ok(c)
}

pub fn router(st: Arc<SiteState>) -> Router {
    Router::new()
        .route("/site/solutions/:cat", get(panel))
        .route("/site/recommend", post(recommend))
        .route("/site/contact", get(contact_get).post(contact_post))
        .route("/admin/leads", get(admin_leads))
        .route("/admin/leads.csv", get(admin_leads_csv))
        .with_state(st)
}

/// Middleware: on the company domain, "/" is the landing page and www redirects to the apex.
pub async fn host_mw(State(st): State<Arc<SiteState>>, req: Request, next: Next) -> Response {
    if st.hosts.is_empty() { return next.run(req).await; }
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("")
        .split(':').next().unwrap_or("").to_lowercase();
    if !st.hosts.iter().any(|h| *h == host) { return next.run(req).await; }
    if let Some(apex) = host.strip_prefix("www.") {
        if st.hosts.iter().any(|h| h == apex) {
            let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
            return Redirect::permanent(&format!("https://{apex}{pq}")).into_response();
        }
    }
    if req.uri().path() == "/" { return home(&st); }
    next.run(req).await
}

// ---------- content ----------

struct Sol { cat: &'static str, title: &'static str, tag: &'static str, desc: &'static str, items: &'static [(&'static str, &'static str)] }

const SOLUTIONS: &[Sol] = &[
    Sol { cat: "kubernetes", title: "Kubernetes & Cloud Native", tag: "Civo Kubernetes · Jakarta",
        desc: "Managed Kubernetes dari Civo di region Jakarta, dioperasikan Arupa sebagai Civo Indonesia Representative. Kubernetes standar (CNCF certified), jadi workload mudah dipindah.",
        items: &[("Civo Kubernetes", "Cluster siap pakai dalam hitungan menit, region Jakarta (JKT1)"),
                 ("Arupa Object Storage (S3)", "Penyimpanan objek S3-compatible untuk aplikasi dan backup"),
                 ("Cloud Stack", "VMware dan Kubernetes untuk aplikasi tradisional maupun modern"),
                 ("GPU / AI", "Komputasi GPU untuk AI, ML dan HPC")] },
    Sol { cat: "compute", title: "Komputasi & Private Cloud", tag: "Virtual Datacenter",
        desc: "Virtual Datacenter yang bisa diatur penuh, sampai private cloud dedicated di data center Jakarta atau Cibitung.",
        items: &[("Virtual Datacenter (VDC)", "Resource custom, HA, backup dan DR terintegrasi, tanpa kontrak mengikat"),
                 ("Dedicated Private Cloud", "Kontrol data penuh dan kepatuhan untuk lingkungan dedicated"),
                 ("Datacenter & Connectivity", "Layanan data center, konektivitas, network & security"),
                 ("Cloud Management", "Monitoring infrastruktur dan aplikasi")] },
    Sol { cat: "backup", title: "Backup & Disaster Recovery", tag: "Veeam · Zerto",
        desc: "Lindungi data di cloud, on-premise dan Microsoft 365 dengan backup terjadwal, immutable, dan DR yang teruji.",
        items: &[("Veeam Backup & Backup 365", "Backup VM, server fisik dan Microsoft 365 ke cloud lokal"),
                 ("Arupa Backup & S3 Object Storage", "Repository backup hemat dengan dukungan immutable"),
                 ("Zerto SecondSite", "Continuous data protection: RPO dalam detik, RTO dalam menit"),
                 ("Veeam Replication", "Replikasi dan failover/failback yang mudah diuji")] },
    Sol { cat: "security", title: "Keamanan Siber", tag: "SOC as a Service",
        desc: "Lapisan keamanan dari edge sampai operasional: cegah serangan, temukan celah, pantau 24x7.",
        items: &[("Anti-DDoS & WAF", "Lindungi aplikasi web dari serangan volumetrik dan L7"),
                 ("Vulnerability Scanning & Pentest", "Temukan dan tutup celah sebelum dieksploitasi"),
                 ("SOC as a Service", "Pemantauan dan respons insiden keamanan"),
                 ("SSL Certificate", "Sertifikat TLS untuk layanan Anda")] },
    Sol { cat: "managed", title: "Managed & Professional Services", tag: "Dukungan lokal",
        desc: "Tim lokal yang mendesain, mengimplementasi dan mengelola infrastruktur Anda — dengan SLA respons yang jelas.",
        items: &[("Design & Architecture", "Rancangan arsitektur cloud, backup dan DR"),
                 ("Implementation & Project Management", "Migrasi dan implementasi end-to-end"),
                 ("Managed Cloud, Backup & Storage", "Operasional harian diurus tim Arupa"),
                 ("Managed CCTV", "Rekaman CCTV online bersama ACOBA")] },
    Sol { cat: "business", title: "Business Solution", tag: "Produktivitas",
        desc: "Aplikasi bisnis as-a-service untuk kolaborasi dan operasional perusahaan.",
        items: &[("Microsoft 365", "Sebagai Microsoft Tier 1 CSP"),
                 ("Enterprise File Sharing & Virtual Dataroom", "Berbagi dokumen yang aman dan terkontrol"),
                 ("Document & Order Management", "Digitalisasi proses dokumen dan pesanan"),
                 ("Warehouse & Treasury System", "Sistem operasional siap pakai")] },
];

const NEEDS: &[(&str, &str)] = &[
    ("modern", "Modernisasi aplikasi / microservices"),
    ("scale", "Aplikasi harus tahan lonjakan traffic"),
    ("ransomware", "Proteksi dari ransomware & kehilangan data"),
    ("dr", "Bisnis tidak boleh berhenti (DR)"),
    ("residency", "Data harus tetap di Indonesia"),
    ("ai", "Menjalankan AI / GPU"),
    ("security", "Mengamankan aplikasi web"),
    ("m365", "Kolaborasi kantor & email"),
];

fn reco_for(need: &str) -> (&'static str, &'static str, &'static str) {
    match need {
        "modern" => ("Civo Kubernetes", "Kubernetes standar di Jakarta — deploy container tanpa mengurus control plane.", "kubernetes"),
        "scale" => ("Civo Kubernetes + autoscaling", "HPA dan node pool menambah kapasitas saat ramai, lalu turun lagi.", "kubernetes"),
        "ransomware" => ("Veeam + Arupa Object Storage", "Backup terjadwal ke repository immutable di cloud lokal.", "backup"),
        "dr" => ("Zerto SecondSite DR", "Replikasi berkelanjutan: RPO detik, RTO menit, failover teruji.", "backup"),
        "residency" => ("Virtual Datacenter / Private Cloud", "Infrastruktur 100% lokal di Jakarta atau Cibitung, dukungan lokal.", "compute"),
        "ai" => ("GPU / AI", "Komputasi GPU untuk training dan inference.", "kubernetes"),
        "security" => ("Anti-DDoS + WAF + SOC", "Lindungi aplikasi publik dan pantau ancaman.", "security"),
        "m365" => ("Microsoft 365 + Veeam Backup 365", "Produktivitas dengan backup Microsoft 365 di Indonesia.", "business"),
        _ => ("Konsultasi arsitektur", "Tim kami bantu memetakan kebutuhan Anda.", "managed"),
    }
}

// ---------- templates ----------

struct CatTab { cat: &'static str, title: &'static str, active: bool }
struct Need { key: &'static str, label: &'static str }
struct Reco { title: &'static str, why: &'static str, cat: &'static str }

#[derive(Template)]
#[template(path = "site/home.html")]
struct HomeTpl<'a> { civo: &'a str, tabs: Vec<CatTab>, sol: &'a Sol, needs: Vec<Need>, seats: i64, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/panel.html")]
struct PanelTpl<'a> { sol: &'a Sol }

#[derive(Template)]
#[template(path = "site/reco.html")]
struct RecoTpl { recos: Vec<Reco>, needs: String }

#[derive(Template)]
#[template(path = "site/contact.html")]
struct ContactTpl { v: Lead, errors: Vec<String>, interests: Vec<Interest> }

pub struct Interest { label: String, selected: bool }

#[derive(Template)]
#[template(path = "site/contact_done.html")]
struct ContactDoneTpl { name: String }

#[derive(Template)]
#[template(path = "site/admin_leads.html")]
struct AdminLeadsTpl<'a> { logo: &'a str, rows: Vec<LeadRow> }

#[derive(Deserialize, Default, Clone)]
pub struct Lead {
    #[serde(default)] name: String,
    #[serde(default)] email: String,
    #[serde(default)] phone: String,
    #[serde(default)] company: String,
    #[serde(default)] need: String,
    #[serde(default)] message: String,
    #[serde(default)] consent: String,
    #[serde(default)] source: String,
    #[serde(default)] website: String,
}

pub struct LeadRow { id: i64, created: String, name: String, email: String, phone: String, company: String, need: String, message: String, source: String }

fn render<T: Template>(t: T) -> Response {
    match t.render() { Ok(s) => Html(s).into_response(), Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {e}")).into_response() }
}

fn interests(need: &str) -> Vec<Interest> {
    SOLUTIONS.iter().map(|s| s.title).chain(std::iter::once("Workshop & training Kubernetes"))
        .map(|t| Interest { label: t.to_string(), selected: need == t }).collect()
}

fn contact_tpl(v: Lead, errors: Vec<String>) -> ContactTpl {
    let interests = interests(&v.need);
    ContactTpl { v, errors, interests }
}

fn home(st: &SiteState) -> Response {
    let tabs = SOLUTIONS.iter().enumerate().map(|(i, s)| CatTab { cat: s.cat, title: s.title, active: i == 0 }).collect();
    let needs = NEEDS.iter().map(|(k, l)| Need { key: k, label: l }).collect();
    let mut r = render(HomeTpl { civo: crate::cert::LOGO, tabs, sol: &SOLUTIONS[0], needs, seats: st.seats(), v: Lead::default(), errors: vec![], interests: interests("") });
    r.headers_mut().insert(header::CACHE_CONTROL, "public, max-age=60".parse().unwrap());
    r
}

async fn panel(Path(cat): Path<String>) -> Response {
    match SOLUTIONS.iter().find(|s| s.cat == cat) { Some(sol) => render(PanelTpl { sol }), None => StatusCode::NOT_FOUND.into_response() }
}

async fn recommend(body: String) -> Response {
    // Form bodies repeat the key for every ticked box: need=a&need=b
    let mut picked: Vec<String> = vec![];
    for pair in body.split('&') {
        if let Some(v) = pair.strip_prefix("need=") { let v = v.to_string(); if NEEDS.iter().any(|(k, _)| *k == v) && !picked.contains(&v) { picked.push(v); } }
    }
    let mut recos: Vec<Reco> = vec![];
    for n in &picked { let (t, w, c) = reco_for(n); if !recos.iter().any(|r| r.title == t) { recos.push(Reco { title: t, why: w, cat: c }); } }
    let needs = picked.iter().filter_map(|k| NEEDS.iter().find(|(kk, _)| kk == k).map(|(_, l)| *l)).collect::<Vec<_>>().join("; ");
    render(RecoTpl { recos, needs })
}

#[derive(Deserialize, Default)]
struct NeedQ { #[serde(default)] need: String, #[serde(default)] note: String }

async fn contact_get(Query(q): Query<NeedQ>) -> Response {
    let need = SOLUTIONS.iter().find(|s| s.cat == q.need || s.title == q.need).map(|s| s.title.to_string()).unwrap_or_default();
    let message = if q.note.is_empty() { String::new() } else { format!("Kebutuhan: {}", clean(&q.note, 400)) };
    render(contact_tpl(Lead { need, message, ..Default::default() }, vec![]))
}

async fn contact_post(State(st): State<Arc<SiteState>>, Form(mut v): Form<Lead>) -> Response {
    if !v.website.is_empty() { return render(ContactDoneTpl { name: String::new() }); }
    v.name = clean(&v.name, 80); v.email = clean(&v.email, 120).to_lowercase(); v.phone = clean(&v.phone, 24);
    v.company = clean(&v.company, 100); v.need = clean(&v.need, 80); v.message = clean(&v.message, 1500); v.source = clean(&v.source, 120);
    let mut errors = vec![];
    if v.name.chars().count() < 2 { errors.push("Mohon isi nama lengkap.".to_string()); }
    if !v.email.split_once('@').map(|(u, d)| !u.is_empty() && d.contains('.')).unwrap_or(false) { errors.push("Mohon isi email yang valid.".to_string()); }
    if v.company.chars().count() < 2 { errors.push("Mohon isi nama perusahaan.".to_string()); }
    if v.consent.is_empty() { errors.push("Mohon setujui penggunaan data agar tim kami bisa menghubungi Anda.".to_string()); }
    if !errors.is_empty() { return render(contact_tpl(v, errors)); }
    let saved = st.db.as_ref().and_then(|d| d.lock().ok()).map(|c| c.execute(
        "INSERT INTO leads (created_at, name, email, phone, company, need, message, source, consent) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1)",
        params![now() as i64, v.name, v.email, v.phone, v.company, v.need, v.message, v.source]).is_ok()).unwrap_or(false);
    if !saved { return render(contact_tpl(v, vec!["Maaf, permintaan belum tersimpan. Silakan coba lagi.".into()])); }
    println!("Lead: {} ({})", v.company, v.need);
    render(ContactDoneTpl { name: v.name.split_whitespace().next().unwrap_or("").to_string() })
}

// ---------- admin ----------

fn admin_ok(st: &SiteState, headers: &HeaderMap) -> bool {
    use base64::Engine;
    let Some(tok) = st.admin_token.as_deref() else { return false };
    let Some(h) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false };
    let given = if let Some(b) = h.strip_prefix("Bearer ") { b.trim().to_string() }
        else if let Some(b) = h.strip_prefix("Basic ") {
            base64::engine::general_purpose::STANDARD.decode(b.trim()).ok().and_then(|r| String::from_utf8(r).ok())
                .and_then(|s| s.split_once(':').map(|(_, p)| p.to_string())).unwrap_or_default()
        } else { return false };
    given.len() == tok.len() && given.bytes().zip(tok.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
}

fn challenge() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic realm=\"Arupa admin\", charset=\"UTF-8\"")], "Admin login required.\n").into_response()
}

fn lead_rows(st: &SiteState) -> Vec<LeadRow> {
    let Some(c) = st.db.as_ref().and_then(|d| d.lock().ok()) else { return vec![] };
    let Ok(mut s) = c.prepare("SELECT id, created_at, name, email, phone, company, need, message, source FROM leads ORDER BY id DESC") else { return vec![] };
    let rows = s.query_map([], |r| Ok(LeadRow { id: r.get(0)?, created: crate::seats::wib_pub(r.get(1)?), name: r.get(2)?, email: r.get(3)?, phone: r.get(4)?,
        company: r.get(5)?, need: r.get(6)?, message: r.get(7)?, source: r.get(8)? }));
    match rows { Ok(it) => it.filter_map(Result::ok).collect(), Err(_) => vec![] }
}

async fn admin_leads(State(st): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    if !admin_ok(&st, &headers) { return challenge(); }
    let mut r = render(AdminLeadsTpl { logo: crate::cert::LOGO, rows: lead_rows(&st) });
    r.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}

async fn admin_leads_csv(State(st): State<Arc<SiteState>>, headers: HeaderMap) -> Response {
    if !admin_ok(&st, &headers) { return challenge(); }
    let q = |s: &str| { let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() }; format!("\"{}\"", s.replace('"', "\"\"")) };
    let mut out = String::from("id,created_wib,name,email,phone,company,need,message,source\n");
    for r in lead_rows(&st).into_iter().rev() {
        out.push_str(&[r.id.to_string(), r.created, r.name, r.email, r.phone, r.company, r.need, r.message, r.source].iter().map(|x| q(x)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    Response::builder().header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=\"arupacloud-leads.csv\"").header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(out)).unwrap()
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().chars().take(max).collect()
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

