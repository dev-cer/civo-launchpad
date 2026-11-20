//! Workshop landing page and seat reservation for "Beyond the Container" (26 Nov 2026).
//!
//! Server-rendered with Askama, interactive with HTMX, stored in SQLite (one file on the PVC).
//!
//!   GET  /register                  -> landing page (Askama)
//!   GET  /register/form             -> HTMX fragment: the registration form (opens in a sheet)
//!   POST /register/form             -> HTMX fragment: form with errors, or the "seat kept" card
//!   GET  /register/seats            -> HTMX fragment: seats counter
//!   GET  /register/calendar.ics     -> calendar file for the workshop
//!   GET  /register/htmx.min.js      -> htmx 2 (vendored, served from the binary)
//!   GET  /api/register/stats        -> JSON {registered, limit, window}
//!   GET  /admin/registrations       -> admin table (HTTP Basic: admin / ADMIN_TOKEN), live search
//!   GET  /admin/registrations.csv   -> CSV export (same auth)
//!
//! Env: SEATS_DB (default /data/register.db), REGISTER_LIMIT (0 = no limit), ADMIN_TOKEN,
//!      REGISTER_CLOSE (RFC3339, default 2026-11-25T23:59:59+07:00): reservations close then,
//!      independent of EVENT_OPEN / EVENT_CLOSE.
//!
//! The reservation only keeps a seat. On race day participants open Launchpad and sign in with the
//! same email; the Launchpad registration (Civo account, Mission Agent token) happens there.

use askama::Template;
use axum::{
    extract::{Form, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Json, Router,
};
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::{env, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};

const HTMX: &str = include_str!("assets/htmx.min.js");
const EVENT_LABEL: &str = "26 NOV · Beyond the Container (workshop)";

pub struct SeatState {
    db: Option<Mutex<Connection>>,
    admin_token: Option<String>,
    window: Option<(u64, u64)>,
    limit: i64,
}

impl SeatState {
    pub fn from_env(admin_token: Option<String>, window: Option<(u64, u64)>) -> Arc<Self> {
        let path = env::var("SEATS_DB").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            // Next to the registrations JSONL when that lives on the PVC, otherwise /data.
            env::var("REGISTRATIONS_FILE").ok().and_then(|f| std::path::Path::new(&f).parent().map(|d| d.join("register.db").to_string_lossy().into_owned()))
                .unwrap_or_else(|| "/data/register.db".to_string())
        });
        let db = match open(&path) {
            Ok(c) => { println!("Seats DB: {path}"); Some(Mutex::new(c)) }
            Err(e) => { eprintln!("Seats DB unavailable at {path}: {e}"); None }
        };
        let limit = env::var("REGISTER_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        Arc::new(SeatState { db, admin_token, window, limit })
    }
    fn closed(&self) -> bool { now() > crate::register_close() }
    fn count(&self) -> i64 {
        self.db.as_ref().and_then(|d| d.lock().ok().and_then(|c| c.query_row("SELECT COUNT(*) FROM seats", [], |r| r.get(0)).ok())).unwrap_or(0)
    }
}

fn open(path: &str) -> rusqlite::Result<Connection> {
    if let Some(dir) = std::path::Path::new(path).parent() { let _ = std::fs::create_dir_all(dir); }
    let c = Connection::open(path)?;
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=3000;
         CREATE TABLE IF NOT EXISTS seats (
            seat         INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            name         TEXT NOT NULL,
            email        TEXT NOT NULL UNIQUE,
            phone        TEXT NOT NULL DEFAULT '',
            company      TEXT NOT NULL DEFAULT '',
            role         TEXT NOT NULL DEFAULT '',
            experience   TEXT NOT NULL DEFAULT '',
            civo_account TEXT NOT NULL DEFAULT '',
            source       TEXT NOT NULL DEFAULT '',
            event        TEXT NOT NULL DEFAULT '',
            consent      INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    Ok(c)
}

pub fn router(st: Arc<SeatState>) -> Router {
    Router::new()
        .route("/register", get(page))
        .route("/register/", get(page))
        .route("/register/form", get(form_get).post(form_post))
        .route("/register/seats", get(seats_fragment))
        .route("/register/calendar.ics", get(ics))
        .route("/register/htmx.min.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript"), (header::CACHE_CONTROL, "public, max-age=86400")], HTMX) }))
        .route("/api/register/stats", get(stats))
        .route("/admin/registrations", get(admin_page))
        .route("/admin/registrations/rows", get(admin_rows))
        .route("/admin/registrations.csv", get(admin_csv))
        .with_state(st)
}

// ---------- templates ----------

#[derive(Template)]
#[template(path = "register/page.html")]
struct PageTpl<'a> { logo: &'a str, closed: bool }

#[derive(Template)]
#[template(path = "register/form.html")]
struct FormTpl { v: Values, errors: Vec<String>, roles: Vec<Opt>, exps: Vec<Opt>, full: bool }

#[derive(Template)]
#[template(path = "register/done.html")]
struct DoneTpl { name: String, email: String, seat: i64, returning: bool }

#[derive(Template)]
#[template(path = "register/closed.html")]
struct ClosedTpl { message: String }

#[derive(Template)]
#[template(path = "register/seats.html")]
struct SeatsTpl { registered: i64, left: Option<i64> }

#[derive(Template)]
#[template(path = "register/admin.html")]
struct AdminTpl<'a> { logo: &'a str, rows: Vec<Row>, total: i64, limit: i64, q: String }

#[derive(Template)]
#[template(path = "register/admin_rows.html")]
struct AdminRowsTpl { rows: Vec<Row> }

struct Opt { value: String, label: String, selected: bool }

#[derive(Deserialize, Default, Clone)]
pub struct Values {
    #[serde(default)] name: String,
    #[serde(default)] email: String,
    #[serde(default)] phone: String,
    #[serde(default)] company: String,
    #[serde(default)] role: String,
    #[serde(default)] experience: String,
    #[serde(default)] civo: String,
    #[serde(default)] consent: String,
    #[serde(default)] source: String,
    /// Honeypot: real people never fill this hidden field.
    #[serde(default)] website: String,
}

pub struct Row {
    seat: i64, created: String, name: String, email: String, phone: String, company: String,
    role: String, experience: String, civo_account: String, source: String,
}

const ROLES: [&str; 6] = ["DevOps / SRE / Platform", "Software engineer", "CTO / Engineering lead", "IT / Infrastructure", "Student", "Other"];
const EXPS: [(&str, &str); 4] = [("new", "New to Kubernetes"), ("some", "Used it a bit"), ("weekly", "Use it every week"), ("prod", "Run it in production")];

fn form_tpl(v: Values, errors: Vec<String>, full: bool) -> FormTpl {
    let roles = ROLES.iter().map(|r| Opt { value: r.to_string(), label: r.to_string(), selected: v.role == *r }).collect();
    let exps = EXPS.iter().map(|(k, l)| Opt { value: k.to_string(), label: l.to_string(), selected: v.experience == *k }).collect();
    FormTpl { v, errors, roles, exps, full }
}

fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(s) => Html(s).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {e}")).into_response(),
    }
}

// ---------- handlers ----------

async fn page(State(st): State<Arc<SeatState>>) -> Response {
    render(PageTpl { logo: crate::cert::LOGO, closed: st.closed() })
}

/// The Mission Race landing page, for "/" on the workshop domain (see site::host_mw).
pub fn landing(closed: bool) -> Response {
    render(PageTpl { logo: crate::cert::LOGO, closed })
}

async fn form_get(State(st): State<Arc<SeatState>>, headers: HeaderMap, Query(q): Query<Values>) -> Response {
    if headers.get("hx-request").is_none() { return Redirect::to("/register#register").into_response(); }
    if st.closed() { return render(ClosedTpl { message: "Registration for 26 Nov is closed. Follow Civo Indonesia for the next workshop.".into() }); }
    if st.limit > 0 && st.count() >= st.limit {
        return render(ClosedTpl { message: "All seats for 26 Nov are taken. Follow Civo Indonesia for the next workshop.".into() });
    }
    render(form_tpl(Values { source: q.source, ..Default::default() }, vec![], st.limit > 0))
}

async fn form_post(State(st): State<Arc<SeatState>>, Form(mut v): Form<Values>) -> Response {
    let full = st.limit > 0;
    if !v.website.is_empty() { return render(DoneTpl { name: "there".into(), email: String::new(), seat: 0, returning: false }); }
    v.name = clean(&v.name, 80);
    v.email = clean(&v.email, 120).to_lowercase();
    v.phone = clean(&v.phone, 24);
    v.company = clean(&v.company, 100);
    v.role = clean(&v.role, 60);
    v.experience = clean(&v.experience, 20);
    v.civo = clean(&v.civo, 120).to_lowercase();
    v.source = clean(&v.source, 120);
    let mut errors = vec![];
    if v.name.chars().count() < 2 { errors.push("Please enter your full name.".to_string()); }
    let ok_email = v.email.split_once('@').map(|(u, d)| !u.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')).unwrap_or(false);
    if !ok_email { errors.push("Please enter a valid email address.".to_string()); }
    if v.company.chars().count() < 2 { errors.push("Please enter your company or campus.".to_string()); }
    if v.consent.is_empty() { errors.push("Please agree to the data consent so we can send your seat details.".to_string()); }
    if !errors.is_empty() { return render(form_tpl(v, errors, full)); }
    if st.closed() { return render(ClosedTpl { message: "Registration for 26 Nov is closed.".into() }); }

    let Some(db) = st.db.as_ref() else {
        return render(form_tpl(v, vec!["Registration is temporarily unavailable. Please try again in a minute.".into()], full));
    };
    let t = now() as i64;
    let res: rusqlite::Result<(i64, bool)> = (|| {
        let c = db.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
        if let Some(seat) = c.query_row("SELECT seat FROM seats WHERE email = ?1", params![v.email], |r| r.get::<_, i64>(0)).optional()? {
            // Same email again: keep the seat number, refresh the details.
            c.execute("UPDATE seats SET updated_at=?1, name=?2,
                        phone=CASE WHEN ?3 = '' THEN phone ELSE ?3 END, company=CASE WHEN ?4 = '' THEN company ELSE ?4 END,
                        role=CASE WHEN ?5 = '' THEN role ELSE ?5 END, experience=CASE WHEN ?6 = '' THEN experience ELSE ?6 END,
                        civo_account=CASE WHEN ?7 = '' THEN civo_account ELSE ?7 END WHERE seat=?8",
                params![t, v.name, v.phone, v.company, v.role, v.experience, v.civo, seat])?;
            return Ok((seat, true));
        }
        if st.limit > 0 {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM seats", [], |r| r.get(0))?;
            if n >= st.limit { return Err(rusqlite::Error::QueryReturnedNoRows); }
        }
        c.execute("INSERT INTO seats (created_at, updated_at, name, email, phone, company, role, experience, civo_account, source, event, consent)
                   VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
            params![t, v.name, v.email, v.phone, v.company, v.role, v.experience, v.civo, v.source, EVENT_LABEL])?;
        Ok((c.last_insert_rowid(), false))
    })();
    match res {
        Ok((seat, returning)) => {
            println!("Seat #{seat} {} ({})", if returning { "updated" } else { "reserved" }, v.source);
            render(DoneTpl { name: v.name.split_whitespace().next().unwrap_or("").to_string(), email: v.email, seat, returning })
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => render(ClosedTpl { message: "All seats for 26 Nov were just taken. Follow Civo Indonesia for the next workshop.".into() }),
        Err(e) => {
            eprintln!("seat insert failed: {e}");
            render(form_tpl(v, vec!["Could not save your seat. Please try again.".into()], full))
        }
    }
}

async fn seats_fragment(State(st): State<Arc<SeatState>>) -> Response {
    let n = st.count();
    render(SeatsTpl { registered: n, left: (st.limit > 0).then(|| (st.limit - n).max(0)) })
}

async fn stats(State(st): State<Arc<SeatState>>) -> impl IntoResponse {
    let w = if st.closed() { "closed" } else { "open" };
    Json(serde_json::json!({ "status": "success", "registered": st.count(), "limit": st.limit, "window": w }))
}

async fn ics() -> impl IntoResponse {
    let body = [
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Civo Indonesia//Launchpad//EN", "BEGIN:VEVENT",
        "UID:btc-20261126@civo-launchpad", "DTSTAMP:20261119T000000Z", "DTSTART:20261126T030000Z", "DTEND:20261126T050000Z",
        "SUMMARY:Beyond the Container - Civo Mission Race", "DESCRIPTION:Bring your laptop. Check-in opens 09:30 WIB.",
        "END:VEVENT", "END:VCALENDAR",
    ].join("\r\n");
    ([(header::CONTENT_TYPE, "text/calendar; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"beyond-the-container.ics\"")], body)
}

// ---------- admin ----------

fn admin_ok(st: &SeatState, headers: &HeaderMap) -> bool {
    let Some(tok) = st.admin_token.as_deref() else { return false };
    let Some(h) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false };
    if let Some(b) = h.strip_prefix("Bearer ") { return ct_eq(b.trim(), tok); }
    if let Some(b) = h.strip_prefix("Basic ") {
        if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b.trim()) {
            if let Ok(s) = String::from_utf8(raw) {
                if let Some((_, pass)) = s.split_once(':') { return ct_eq(pass, tok); }
            }
        }
    }
    false
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn challenge() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic realm=\"Civo Launchpad admin\", charset=\"UTF-8\"")],
     "Admin login required. User: admin, password: the Launchpad admin token.\n").into_response()
}

#[derive(Deserialize, Default)]
struct AdminQ { #[serde(default)] q: String }

fn query_rows(st: &SeatState, q: &str) -> Vec<Row> {
    let Some(db) = st.db.as_ref() else { return vec![] };
    let Ok(c) = db.lock() else { return vec![] };
    let like = format!("%{}%", q.trim().to_lowercase());
    let Ok(mut stmt) = c.prepare(
        "SELECT seat, created_at, name, email, phone, company, role, experience, civo_account, source FROM seats
         WHERE ?1 = '%%' OR lower(name) LIKE ?1 OR email LIKE ?1 OR lower(company) LIKE ?1 OR lower(source) LIKE ?1
         ORDER BY seat DESC") else { return vec![] };
    let rows = stmt.query_map(params![like], |r| Ok(Row {
        seat: r.get(0)?, created: wib(r.get::<_, i64>(1)?), name: r.get(2)?, email: r.get(3)?, phone: r.get(4)?,
        company: r.get(5)?, role: r.get(6)?, experience: r.get(7)?, civo_account: r.get(8)?, source: r.get(9)?,
    }));
    match rows { Ok(it) => it.filter_map(Result::ok).collect(), Err(_) => vec![] }
}

async fn admin_page(State(st): State<Arc<SeatState>>, headers: HeaderMap, Query(q): Query<AdminQ>) -> Response {
    if !admin_ok(&st, &headers) { return challenge(); }
    let rows = query_rows(&st, &q.q);
    let mut r = render(AdminTpl { logo: crate::cert::LOGO, total: st.count(), limit: st.limit, rows, q: q.q });
    r.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}

async fn admin_rows(State(st): State<Arc<SeatState>>, headers: HeaderMap, Query(q): Query<AdminQ>) -> Response {
    if !admin_ok(&st, &headers) { return challenge(); }
    render(AdminRowsTpl { rows: query_rows(&st, &q.q) })
}

async fn admin_csv(State(st): State<Arc<SeatState>>, headers: HeaderMap) -> Response {
    if !admin_ok(&st, &headers) { return challenge(); }
    let mut out = String::from("seat,registered_wib,name,email,phone,company,role,experience,civo_account,source\n");
    for r in query_rows(&st, "").into_iter().rev() {
        let f = [r.seat.to_string(), r.created, r.name, r.email, r.phone, r.company, r.role, r.experience, r.civo_account, r.source];
        out.push_str(&f.iter().map(|x| csv(x)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    ([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"beyond-the-container-seats.csv\""),
      (header::CACHE_CONTROL, "no-store")], out).into_response()
}

fn csv(s: &str) -> String {
    // Quote always; neutralise spreadsheet formulas.
    let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() };
    format!("\"{}\"", s.replace('"', "\"\""))
}

// ---------- helpers ----------

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(max).collect()
}

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

pub fn wib_pub(t: i64) -> String { wib(t) }

/// Unix seconds -> "YYYY-MM-DD HH:MM" in WIB (UTC+7).
fn wib(t: i64) -> String {
    let t = t + 7 * 3600;
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    let z = days + 719468; let era = z.div_euclid(146097); let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}
