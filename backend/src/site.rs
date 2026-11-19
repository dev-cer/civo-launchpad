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
    /// What "/" shows on the site hosts: "workshop" (register landing, default) or "company".
    home: String,
    window: Option<(u64, u64)>,
    db: Option<Mutex<Connection>>,
    admin_token: Option<String>,
}

impl SiteState {
    pub fn from_env(admin_token: Option<String>, window: Option<(u64, u64)>) -> Arc<Self> {
        let hosts: Vec<String> = env::var("SITE_HOSTS").unwrap_or_default().split(',')
            .map(|h| h.trim().to_lowercase()).filter(|h| !h.is_empty()).collect();
        let path = env::var("SEATS_DB").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/data/register.db".to_string());
        let db = open(&path).map(Mutex::new).map_err(|e| eprintln!("Leads DB unavailable at {path}: {e}")).ok();
        if !hosts.is_empty() { println!("Site hosts: {}", hosts.join(", ")); }
        let home = env::var("SITE_HOME").unwrap_or_default().trim().to_lowercase();
        // "/" on the company domain: the Arupa Cloud page (default) or the workshop landing.
        let home = if home == "workshop" { home } else { "company".to_string() };
        Arc::new(SiteState { hosts, db, admin_token, home, window })
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
        .route("/civo", get(civo_page))
        .route("/civo/", get(civo_page))
        .route("/civo/partner", get(partner_page))
        .route("/civo/kubernetes", get(civo_page))
        .route("/civo/ai", get(|| product_page("civo-ai")))
        .route("/civo/private", get(|| product_page("civo-private")))
        .route("/events", get(events_page))
        .route("/about", get(about_page))
        .route("/about-us", get(|| async { Redirect::permanent("/about") }))
        .route("/events/", get(events_page))
        .route("/event", get(|| async { Redirect::permanent("/events") }))
        .route("/partnercivo", get(|| async { Redirect::permanent("/civo/partner") }))
        .route("/partner-civo", get(|| async { Redirect::permanent("/civo/partner") }))
        .route("/site/solutions/:cat", get(panel))
        .route("/site/recommend", post(recommend))
        .route("/site/contact", get(contact_get).post(contact_post))
        .route("/brand/arupa-logo.png", get(|| async { png(include_bytes!("assets/arupa-logo.png")) }))
        .route("/brand/arupa-logo-white.png", get(|| async { png(include_bytes!("assets/arupa-logo-white.png")) }))
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
    if req.uri().path() == "/" {
        if st.home == "company" { return home(&st); }
        // Workshop domain: "/" is the Mission Race register landing page. Rendered here, because
        // Router::layer middleware runs after routing (rewriting the URI would hit the static fallback).
        let closed = matches!(st.window, Some((_, c)) if now() > c);
        return crate::seats::landing(closed);
    }
    next.run(req).await
}

// ---------- content ----------

struct Sol { cat: &'static str, title: &'static str, tag: &'static str, desc: &'static str, items: &'static [(&'static str, &'static str)] }

const SOLUTIONS: &[Sol] = &[
    Sol { cat: "cloud", title: "Cloud Infrastructure", tag: "Sovereign cloud · Jakarta & Cibitung",
        desc: "Cloud di data center Indonesia — dari Virtual Datacenter dan private cloud sampai Kubernetes terkelola Civo di region Jakarta.",
        items: &[("Zettagrid Cloud Platform", "Virtual Datacenter: resource custom, HA, backup dan DR terintegrasi"),
                 ("Civo Cloud Platform", "Kubernetes terkelola di region Jakarta (JKT1), siap dalam ~90 detik"),
                 ("VMware Cloud Foundation", "Private cloud berbasis VMware — Broadcom Premier Partner (VCSP)"),
                 ("Red Hat Hybrid Cloud", "Platform hybrid cloud untuk aplikasi enterprise"),
                 ("SUSE Cloud Native", "Stack cloud-native terbuka"),
                 ("Dell & HPE Server", "Server dan storage untuk on-premise dan private cloud")] },
    Sol { cat: "resilience", title: "Business Resilience", tag: "Backup · DR · Veeam",
        desc: "Backup immutable dan disaster recovery antarzona, supaya bisnis tetap berjalan saat ransomware, bencana atau downtime.",
        items: &[("Zettagrid Cloud Backup", "Backup VM dan server ke cloud lokal"),
                 ("Zettagrid Cloud DR", "SecondSite DR: RPO dalam detik, RTO dalam menit"),
                 ("Arupa Managed Backup", "Backup yang dioperasikan tim Arupa"),
                 ("Veeam Data Platform", "Backup dan replikasi — Veeam Platinum Cloud Provider"),
                 ("Veeam Backup for Microsoft 365", "Proteksi email dan file Microsoft 365 di Indonesia"),
                 ("Arupa CCTV Backup", "Rekaman CCTV online bersama Acoba")] },
    Sol { cat: "aidata", title: "AI & Data Platform", tag: "Object storage · lakehouse · GPU",
        desc: "Fondasi data dan AI generasi baru: object storage S3, data lakehouse dan komputasi AI.",
        items: &[("MinIO AIStor", "Object storage berperforma tinggi untuk AI"),
                 ("Dremio Data Lakehouse", "Query terpadu lintas data lake dan database"),
                 ("Croit Ceph Storage", "Software-defined storage berbasis Ceph"),
                 ("Arupa Object Storage", "S3-compatible, cocok untuk backup immutable"),
                 ("Supermicro AI Computing", "Server GPU untuk training dan inference")] },
    Sol { cat: "specialized", title: "Specialized Solutions", tag: "Keamanan · jaringan · platform",
        desc: "Solusi pelengkap untuk keamanan, jaringan dan operasional container.",
        items: &[("Portainer", "Manajemen container dan Kubernetes yang sederhana"),
                 ("HPE Networking", "Jaringan enterprise"),
                 ("Fortinet Security", "Firewall dan keamanan jaringan"),
                 ("Palo Alto AI Security", "Keamanan berbasis AI"),
                 ("Cloudflare Web Security", "WAF, Anti-DDoS dan proteksi aplikasi web"),
                 ("Twingate Zero Trust", "Akses jarak jauh zero trust"),
                 ("Kaspersky Security", "Proteksi endpoint")] },
];

/// Extra interests for the consultation form (not pillars).
const EXTRA_NEEDS: &[(&str, &str)] = &[("civo", "Civo Kubernetes"), ("civo-ai", "Civo AI (GPU)"), ("civo-private", "Civo Private (CivoStack)"),
    ("partner", "Program Partner Civo"), ("workshop", "Workshop & training Kubernetes"), ("event", "Undangan event berikutnya")];

pub struct Prod {
    key: &'static str, title: &'static str, h1a: &'static str, h1b: &'static str, tagline: &'static str, avail: &'static str,
    accent: &'static str, glow: &'static str, cta: &'static str, flow_title: &'static str, flow: &'static [(&'static str, &'static str, &'static str)],
    feat_title: &'static str, features: &'static [(&'static str, &'static str)], cases: &'static [(&'static str, &'static str)],
    road_title: &'static str, road: &'static [(&'static str, &'static str, &'static str)], note: &'static str,
}

const PRODUCTS: &[Prod] = &[
    Prod { key: "civo-ai", title: "Civo AI", h1a: "GPU cloud untuk", h1b: "membangun, melatih dan merilis AI.",
        tagline: "Jalankan training dan inference AI dengan performa GPU NVIDIA yang bisa diskalakan. GPU untuk region Indonesia segera hadir (coming soon) — daftarkan minat Anda sekarang.",
        avail: "GPU region Indonesia: coming soon · daftar minat", accent: "#7cf3ff", glow: "rgba(124,243,255,.18)", cta: "Daftar minat GPU",
        flow_title: "ALUR AI DI CIVO", flow: &[("🛠️", "Build", "Siapkan data dan environment di Kubernetes: notebook, pipeline dan storage S3."),
            ("🧠", "Train", "Latih atau fine-tune model dengan GPU NVIDIA; tambah node GPU saat dibutuhkan."),
            ("🚀", "Ship", "Sajikan model sebagai API di cluster yang sama dan skalakan sesuai trafik.")],
        feat_title: "AI di cloud yang Anda kenal",
        features: &[("⚡ GPU NVIDIA", "Performa GPU untuk training dan inference, bisa diskalakan sesuai kebutuhan."),
            ("☸️ Kubernetes-native", "GPU sebagai node pool di cluster Civo — tool dan workflow yang sama."),
            ("🇮🇩 Data di Indonesia", "Region lokal yang dioperasikan Arupa, cocok untuk data sensitif."),
            ("📚 Siap dipelajari", "Panduan Civo Learn: Kubeflow, Ollama, fine-tuning LLM, OCR dengan GPU."),
            ("🗄️ Storage untuk AI", "Padukan dengan Arupa Object Storage atau MinIO AIStor."),
            ("🌐 Tanpa biaya egress", "Pindahkan data dan hasil model tanpa biaya transfer keluar.")],
        cases: &[("LLM privat untuk dokumen internal", "Jalankan LLM open-source (misalnya dengan Ollama) di cluster Anda sendiri, sehingga dokumen tidak keluar dari Indonesia."),
            ("Fine-tuning model", "Sesuaikan model bahasa dengan data dan istilah bisnis Anda menggunakan GPU sesuai kebutuhan."),
            ("OCR dan computer vision", "Proses dokumen dan gambar dalam jumlah besar dengan pipeline GPU."),
            ("Inference API", "Sajikan model sebagai layanan yang otomatis bertambah kapasitasnya saat permintaan naik.")],
        road_title: "Dari daftar minat sampai produksi",
        road: &[("HARI INI", "Daftar minat", "Ceritakan kebutuhan GPU Anda lewat form di bawah."), ("BERIKUTNYA", "Sizing", "Tim Arupa membantu memilih tipe dan jumlah GPU."),
            ("COMING SOON", "GPU live", "Kapasitas GPU tersedia di region Indonesia."), ("SETELAHNYA", "Produksi", "Workload AI berjalan dengan dukungan tim lokal.")],
        note: "Ketersediaan, tipe GPU dan harga dikonfirmasi saat sizing. Skema GPU terpisah dari Civo Kubernetes." },
    Prod { key: "civo-private", title: "Civo Private", h1a: "Private cloud berdaulat,", h1b: "single-tenant.",
        tagline: "CivoStack Enterprise: software stack Civo untuk private dan edge cloud di hardware Anda, partner, atau data center — pengalaman cloud yang sama dengan Civo public, dengan kendali penuh.",
        avail: "Tanyakan ketersediaannya", accent: "#60a5fa", glow: "rgba(96,165,250,.22)", cta: "Diskusikan private cloud",
        flow_title: "JALANKAN DI MANA SAJA", flow: &[("🏢", "Data center Anda", "CivoStack berjalan di hardware Anda sendiri untuk kendali penuh atas data dan operasional."),
            ("🤝", "Partner / Arupa", "Di data center partner atau Arupa, dengan operasi dan dukungan tim lokal."),
            ("📡", "Edge", "Cloud di lokasi edge untuk latensi rendah dan data yang tetap lokal.")],
        feat_title: "Cloud-native, tapi tetap milik Anda",
        features: &[("🔒 Berdaulat & single-tenant", "Infrastruktur khusus untuk Anda, tanpa berbagi dengan tenant lain."),
            ("🧱 CivoStack", "Software stack buatan Civo, dirancang dari nol dengan prinsip cloud-native."),
            ("🧰 Pengalaman yang sama", "Dashboard, CLI, API dan Terraform seperti di Civo public."),
            ("♻️ Modernisasi", "Jalur dari virtualisasi lama ke platform Kubernetes yang modern."),
            ("📜 Kepatuhan", "Data dan operasional berada di lokasi yang Anda tentukan."),
            ("🛟 Didampingi Arupa", "Assessment, migrasi dan operasional 24x7 dari tim lokal.")],
        cases: &[("Keluar dari virtualisasi lama", "Modernisasi lingkungan yang kurang lincah ke private cloud Kubernetes-native, bertahap dan terukur."),
            ("Sektor teregulasi", "Pemerintah, BUMN dan layanan keuangan yang mewajibkan lingkungan khusus dan dapat diaudit."),
            ("Edge dan cabang", "Ritel, manufaktur atau lokasi jarak jauh yang membutuhkan cloud dekat dengan data."),
            ("Hosting & MSP", "Penyedia hosting yang ingin menawarkan private cloud sendiri kepada pelanggannya.")],
        road_title: "Langkah menuju private cloud",
        road: &[("01", "Discover", "Assessment workload, risiko dan kepatuhan."), ("02", "Design", "Arsitektur, lokasi dan model biaya."),
            ("03", "PoC", "Validasi workload dengan kriteria sukses."), ("04", "Operate", "Produksi dan operasional bersama tim Arupa.")],
        note: "Arsitektur, lokasi dan kepatuhan divalidasi untuk setiap kebutuhan." },
];

const NEEDS: &[(&str, &str)] = &[
    ("modern", "Modernisasi aplikasi / Kubernetes"),
    ("scale", "Aplikasi harus tahan lonjakan traffic"),
    ("ransomware", "Proteksi dari ransomware & kehilangan data"),
    ("dr", "Bisnis tidak boleh berhenti (DR)"),
    ("residency", "Data harus tetap di Indonesia"),
    ("ai", "Menjalankan AI / data platform"),
    ("security", "Mengamankan aplikasi web"),
    ("m365", "Proteksi Microsoft 365"),
];

fn reco_for(need: &str) -> (&'static str, &'static str, &'static str) {
    match need {
        "modern" => ("Civo Kubernetes", "Kubernetes standar di region Jakarta — deploy container tanpa mengurus control plane.", "civo"),
        "scale" => ("Civo Kubernetes + autoscaling", "HPA dan node pool menambah kapasitas saat ramai, lalu turun lagi.", "civo"),
        "ransomware" => ("Veeam + Arupa Object Storage", "Backup terjadwal ke repository immutable di cloud lokal.", "resilience"),
        "dr" => ("Zettagrid Cloud DR (SecondSite)", "Replikasi antarzona Jakarta–Cibitung: RPO detik, RTO menit.", "resilience"),
        "residency" => ("Sovereign Cloud: VDC / Private Cloud", "Infrastruktur di Jakarta dan Cibitung, dikelola tim lokal 24x7.", "cloud"),
        "ai" => ("MinIO AIStor + GPU", "Object storage untuk AI dan komputasi GPU untuk training dan inference.", "aidata"),
        "security" => ("Cloudflare + Fortinet", "WAF, Anti-DDoS dan firewall untuk aplikasi publik.", "specialized"),
        "m365" => ("Veeam Backup for Microsoft 365", "Backup email dan file Microsoft 365 dengan data di Indonesia.", "resilience"),
        _ => ("Konsultasi arsitektur", "Tim kami bantu memetakan kebutuhan Anda.", "cloud"),
    }
}

// ---------- templates ----------

struct CatTab { cat: &'static str, title: &'static str, active: bool }
struct Need { key: &'static str, label: &'static str }
struct Reco { title: &'static str, why: &'static str, cat: &'static str }

#[derive(Template)]
#[template(path = "site/home.html")]
struct HomeTpl<'a> { civo: &'a str, nav: &'a str, tabs: Vec<CatTab>, sol: &'a Sol, needs: Vec<Need>, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/civo.html")]
struct CivoTpl<'a> { civo: &'a str, nav: &'a str, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/civo_product.html")]
struct ProdTpl<'a> { civo: &'a str, nav: &'a str, p: &'a Prod, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/about.html")]
struct AboutTpl<'a> { civo: &'a str, nav: &'a str, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/events.html")]
struct EventsTpl<'a> { civo: &'a str, nav: &'a str, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

#[derive(Template)]
#[template(path = "site/partner.html")]
struct PartnerTpl<'a> { civo: &'a str, nav: &'a str, v: Lead, errors: Vec<String>, interests: Vec<Interest> }

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
    SOLUTIONS.iter().map(|s| s.title).chain(EXTRA_NEEDS.iter().map(|(_, t)| *t))
        .map(|t| Interest { label: t.to_string(), selected: need == t }).collect()
}
fn need_title(key: &str) -> String {
    SOLUTIONS.iter().find(|s| s.cat == key || s.title == key).map(|s| s.title)
        .or_else(|| EXTRA_NEEDS.iter().find(|(k, t)| *k == key || *t == key).map(|(_, t)| *t))
        .unwrap_or("").to_string()
}

fn contact_tpl(v: Lead, errors: Vec<String>) -> ContactTpl {
    let interests = interests(&v.need);
    ContactTpl { v, errors, interests }
}

fn home(_st: &SiteState) -> Response {
    let tabs = SOLUTIONS.iter().enumerate().map(|(i, s)| CatTab { cat: s.cat, title: s.title, active: i == 0 }).collect();
    let needs = NEEDS.iter().map(|(k, l)| Need { key: k, label: l }).collect();
    let mut r = render(HomeTpl { civo: crate::cert::LOGO, nav: "home", tabs, sol: &SOLUTIONS[0], needs, v: Lead::default(), errors: vec![], interests: interests("") });
    r.headers_mut().insert(header::CACHE_CONTROL, "public, max-age=60".parse().unwrap());
    r
}

async fn civo_page() -> Response {
    let need = need_title("civo");
    render(CivoTpl { civo: crate::cert::LOGO, nav: "civo", interests: interests(&need), v: Lead { need, ..Default::default() }, errors: vec![] })
}

async fn product_page(key: &'static str) -> Response {
    let p = PRODUCTS.iter().find(|p| p.key == key).unwrap_or(&PRODUCTS[0]);
    let need = need_title(p.key);
    render(ProdTpl { civo: crate::cert::LOGO, nav: "civo", p, interests: interests(&need), v: Lead { need, ..Default::default() }, errors: vec![] })
}

async fn about_page() -> Response {
    render(AboutTpl { civo: crate::cert::LOGO, nav: "about", interests: interests(""), v: Lead::default(), errors: vec![] })
}

async fn events_page() -> Response {
    let need = need_title("event");
    render(EventsTpl { civo: crate::cert::LOGO, nav: "events", interests: interests(&need), v: Lead { need, ..Default::default() }, errors: vec![] })
}

async fn partner_page() -> Response {
    let need = need_title("partner");
    render(PartnerTpl { civo: crate::cert::LOGO, nav: "partner", interests: interests(&need), v: Lead { need, ..Default::default() }, errors: vec![] })
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
    let need = need_title(&q.need);
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

fn png(b: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], b).into_response()
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().chars().take(max).collect()
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

