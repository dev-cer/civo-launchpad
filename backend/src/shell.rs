//! Launchpad Shell: satu pod terminal (ttyd) per peserta di cluster launchpad-ops, dibuka di browser.
//!
//! Alur: peserta membuat cluster di dashboard Civo → unggah kubeconfig di Launchpad → pod shell-nya
//! menarik kubeconfig itu (sekali, lewat /api/shell/pull) → kubectl tersambung dan Mission Agent terpasang.
//! Shell tidak pernah memegang API key Civo. Terminal hanya bisa dibuka lewat proxy Launchpad:
//! cookie sesi + basic auth ttyd yang acak per pod, dan NetworkPolicy menutup akses langsung antar pod.
//!
//! Env: SHELL_ENABLED=1, SHELL_IMAGE, SHELL_NAMESPACE (civo-shells), SHELL_MAX (60), SHELL_IDLE_MIN (120),
//!      SHELL_LAUNCHPAD_INTERNAL (http://launchpad.civo-workshop.svc.cluster.local)
use crate::kube::Kube;
use axum::{
    body::Body,
    extract::ws::{Message as AMsg, WebSocket, WebSocketUpgrade},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

pub struct Session {
    pub sid: String,
    pub email: String,
    pub pod: String,
    pub shell_token: String,
    pub cred: String,
    pub created: u64,
    pub last_active: AtomicU64,
}

pub struct Shells {
    pub kube: Option<Kube>,
    pub image: String,
    pub ns: String,
    pub max: usize,
    pub idle_min: u64,
    pub internal_url: String,
    pub sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// sid -> (kubeconfig, join_url): diserahkan sekali ke pod shell, lalu dihapus.
    pub pending: Mutex<HashMap<String, (String, String)>>,
}

fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }
fn rand_hex(n: usize) -> String { (0..n).map(|_| format!("{:x}", uuid::Uuid::new_v4().as_bytes()[0] & 0x0f)).collect::<String>() }
fn secret() -> String { format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()) }
pub fn pod_name(email: &str) -> String {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(email.as_bytes()).iter().take(5).map(|b| format!("{b:02x}")).collect();
    format!("shell-{h}")
}

impl Shells {
    pub fn from_env() -> Shells {
        let on = std::env::var("SHELL_ENABLED").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
        let kube = if on { match Kube::new() { Ok(k) => Some(k), Err(e) => { eprintln!("Launchpad Shell disabled: {e}"); None } } } else { None };
        let env = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| d.to_string());
        Shells {
            kube,
            image: env("SHELL_IMAGE", "ghcr.io/dev-cer/civo-launchpad:shell-latest"),
            ns: env("SHELL_NAMESPACE", "civo-shells"),
            max: env("SHELL_MAX", "60").parse().unwrap_or(60),
            idle_min: env("SHELL_IDLE_MIN", "120").parse().unwrap_or(120),
            internal_url: env("SHELL_LAUNCHPAD_INTERNAL", "http://launchpad.civo-workshop.svc.cluster.local"),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }
    pub fn enabled(&self) -> bool { self.kube.is_some() }
    fn pods_path(&self) -> String { format!("/api/v1/namespaces/{}/pods", self.ns) }

    /// Setelah Launchpad restart: pulihkan sesi dari anotasi pod yang masih berjalan.
    pub async fn restore(&self) {
        let Some(k) = &self.kube else { return };
        match k.get(&format!("{}?labelSelector=app%3Dlaunchpad-shell", self.pods_path())).await {
            Ok(list) => {
                let mut s = self.sessions.lock().await;
                for p in list["items"].as_array().cloned().unwrap_or_default() {
                    let a = &p["metadata"]["annotations"];
                    let (Some(sid), Some(email)) = (a["launchpad.civo.id/sid"].as_str(), a["launchpad.civo.id/email"].as_str()) else { continue };
                    let env = p["spec"]["containers"][0]["env"].as_array().cloned().unwrap_or_default();
                    let get = |n: &str| env.iter().find(|e| e["name"] == n).and_then(|e| e["value"].as_str()).unwrap_or("").to_string();
                    s.insert(sid.to_string(), Arc::new(Session { sid: sid.into(), email: email.into(), pod: p["metadata"]["name"].as_str().unwrap_or("").into(),
                        shell_token: get("SHELL_TOKEN"), cred: get("TTYD_CREDENTIAL"), created: now(), last_active: AtomicU64::new(now()) }));
                }
                if !s.is_empty() { println!("Launchpad Shell: restored {} session(s)", s.len()); }
            }
            Err(e) => eprintln!("Launchpad Shell: cannot list pods: {e}"),
        }
    }

    pub async fn by_email(&self, email: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.values().find(|s| s.email == email).cloned()
    }
    pub async fn by_sid(&self, sid: &str) -> Option<Arc<Session>> { self.sessions.lock().await.get(sid).cloned() }

    /// Buat (atau pakai ulang) pod shell peserta.
    pub async fn start(&self, email: &str) -> Result<Arc<Session>, String> {
        let k = self.kube.as_ref().ok_or("Launchpad Shell is not enabled on this server.")?;
        if let Some(s) = self.by_email(email).await {
            if k.get_opt(&format!("{}/{}", self.pods_path(), s.pod)).await?.is_some() { s.last_active.store(now(), Ordering::Relaxed); return Ok(s); }
            self.sessions.lock().await.remove(&s.sid);
        }
        if self.sessions.lock().await.len() >= self.max { return Err("All Launchpad Shells are in use right now — use your laptop terminal, or ask a facilitator.".into()); }
        // (The ResourceQuota on the namespace is the hard limit when many people start at the same second.)
        let name = pod_name(email);
        // Pod lama tanpa sesi (mis. sesudah restart yang gagal dipulihkan): hapus dulu.
        let _ = k.delete(&format!("{}/{}", self.pods_path(), name)).await;
        for _ in 0..20 { if k.get_opt(&format!("{}/{}", self.pods_path(), name)).await?.is_none() { break; } tokio::time::sleep(std::time::Duration::from_millis(500)).await; }
        let sid = format!("{}{}", rand_hex(6), uuid::Uuid::new_v4().simple());
        let s = Arc::new(Session { sid: sid.clone(), email: email.into(), pod: name.clone(), shell_token: secret(),
            cred: format!("lp:{}", secret()), created: now(), last_active: AtomicU64::new(now()) });
        let pod = json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": name, "labels": { "app": "launchpad-shell" },
                "annotations": { "launchpad.civo.id/sid": sid, "launchpad.civo.id/email": email } },
            "spec": {
                "automountServiceAccountToken": false, "enableServiceLinks": false, "restartPolicy": "Always",
                "terminationGracePeriodSeconds": 2,
                "securityContext": { "runAsNonRoot": true, "runAsUser": 10001, "fsGroup": 10001, "seccompProfile": { "type": "RuntimeDefault" } },
                "containers": [{
                    "name": "shell", "image": self.image,
                    "env": [
                        { "name": "SHELL_ID", "value": sid }, { "name": "SHELL_TOKEN", "value": s.shell_token },
                        { "name": "TTYD_CREDENTIAL", "value": s.cred }, { "name": "LAUNCHPAD_INTERNAL", "value": self.internal_url }
                    ],
                    "ports": [{ "containerPort": 7681, "name": "ttyd" }],
                    "readinessProbe": { "tcpSocket": { "port": 7681 }, "periodSeconds": 2 },
                    "resources": { "requests": { "cpu": "50m", "memory": "96Mi" }, "limits": { "cpu": "500m", "memory": "384Mi", "ephemeral-storage": "512Mi" } },
                    "securityContext": { "allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true, "capabilities": { "drop": ["ALL"] } },
                    "volumeMounts": [{ "name": "home", "mountPath": "/home/learner" }, { "name": "tmp", "mountPath": "/tmp" }]
                }],
                "volumes": [
                    { "name": "home", "emptyDir": { "medium": "Memory", "sizeLimit": "128Mi" } },
                    { "name": "tmp", "emptyDir": { "medium": "Memory", "sizeLimit": "64Mi" } }
                ]
            }
        });
        let full = "All Launchpad Shells are in use right now — use your laptop terminal, or ask a facilitator.";
        let (code, v) = match k.post(&self.pods_path(), &pod).await {
            Ok(x) => x,
            Err(e) if e.contains("exceeded quota") => return Err(full.into()),
            Err(e) => return Err(format!("Could not start your shell: {e}")),
        };
        if code >= 300 { return Err(format!("Could not start your shell (HTTP {code}): {}", v["message"].as_str().unwrap_or(""))); }
        self.sessions.lock().await.insert(sid, s.clone());
        Ok(s)
    }

    /// (siap?, IP pod, pesan bila bermasalah)
    pub async fn pod_state(&self, s: &Session) -> (bool, Option<String>, String) {
        let Some(k) = &self.kube else { return (false, None, "disabled".into()) };
        match k.get_opt(&format!("{}/{}", self.pods_path(), s.pod)).await {
            Ok(Some(p)) => {
                let ready = p["status"]["containerStatuses"][0]["ready"] == true;
                let ip = p["status"]["podIP"].as_str().map(String::from);
                let waiting = p["status"]["containerStatuses"][0]["state"]["waiting"]["reason"].as_str().unwrap_or("").to_string();
                let sched = p["status"]["conditions"].as_array().and_then(|c| c.iter().find(|x| x["type"] == "PodScheduled" && x["status"] == "False"))
                    .and_then(|x| x["message"].as_str()).unwrap_or("").to_string();
                (ready, ip, if !waiting.is_empty() { waiting } else { sched })
            }
            Ok(None) => (false, None, "gone".into()),
            Err(e) => (false, None, e),
        }
    }

    pub async fn end(&self, s: &Session) {
        if let Some(k) = &self.kube { let _ = k.delete(&format!("{}/{}", self.pods_path(), s.pod)).await; }
        self.sessions.lock().await.remove(&s.sid);
        self.pending.lock().await.remove(&s.sid);
    }

    /// Hapus shell yang menganggur, atau semuanya setelah event ditutup.
    pub async fn sweep(&self, closed: bool) -> usize {
        let idle = self.idle_min * 60;
        let victims: Vec<Arc<Session>> = self.sessions.lock().await.values()
            .filter(|s| closed || now().saturating_sub(s.last_active.load(Ordering::Relaxed)) > idle).cloned().collect();
        for s in &victims { self.end(s).await; }
        victims.len()
    }
}

fn cookie_ok(headers: &HeaderMap, sid: &str) -> bool {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';')).any(|c| c.trim() == format!("lp_shell={sid}"))
}

pub fn set_cookie(sid: &str, secure: bool) -> HeaderValue {
    HeaderValue::from_str(&format!("lp_shell={sid}; Path=/shell/{sid}/; HttpOnly; SameSite=Strict; Max-Age=86400{}", if secure { "; Secure" } else { "" })).unwrap()
}

fn basic(cred: &str) -> String {
    use base64::Engine;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(cred))
}

fn plain(code: StatusCode, msg: &str) -> Response<Body> {
    Response::builder().status(code).header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(format!("<!doctype html><body style=\"background:#0b0b0b;color:#ddd;font-family:sans-serif;padding:24px\">{msg}</body>"))).unwrap()
}

/// Proxy HTTP + WebSocket ke ttyd di pod shell. `rest` = path setelah /shell/<sid>/.
pub async fn proxy(shells: Arc<Shells>, sid: String, rest: String, headers: HeaderMap, ws: Option<WebSocketUpgrade>) -> Response<Body> {
    let Some(s) = shells.by_sid(&sid).await else { return plain(StatusCode::NOT_FOUND, "This shell session has ended. Open the Terminal tab in Launchpad again.") };
    if !cookie_ok(&headers, &sid) { return plain(StatusCode::FORBIDDEN, "Open your shell from the Terminal tab in Launchpad.") }
    let (ready, ip, why) = shells.pod_state(&s).await;
    let Some(ip) = ip.filter(|_| ready) else { return plain(StatusCode::SERVICE_UNAVAILABLE, &format!("Your shell is still starting… {why}")) };
    s.last_active.store(now(), Ordering::Relaxed);
    let upstream = format!("{ip}:7681/shell/{sid}/{rest}");
    if let Some(ws) = ws {
        let cred = s.cred.clone();
        let sess = s.clone();
        return ws.protocols(["tty"]).on_upgrade(move |socket| relay(socket, format!("ws://{upstream}"), cred, sess)).into_response();
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().unwrap();
    match client.get(format!("http://{upstream}")).header(header::AUTHORIZATION, basic(&s.cred)).send().await {
        Ok(r) => {
            let code = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let ct = r.headers().get(header::CONTENT_TYPE).cloned();
            let body = r.bytes().await.unwrap_or_default();
            let mut b = Response::builder().status(code).header(header::CACHE_CONTROL, "no-store");
            if let Some(ct) = ct { b = b.header(header::CONTENT_TYPE, ct); }
            b.body(Body::from(body)).unwrap()
        }
        Err(e) => plain(StatusCode::BAD_GATEWAY, &format!("Could not reach your shell: {e}")),
    }
}

async fn relay(client: WebSocket, url: String, cred: String, sess: Arc<Session>) {
    let mut req = match url.into_client_request() { Ok(r) => r, Err(_) => return };
    req.headers_mut().insert(header::AUTHORIZATION, HeaderValue::from_str(&basic(&cred)).unwrap());
    req.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static("tty"));
    let Ok((up, _)) = tokio_tungstenite::connect_async(req).await else { return };
    let (mut up_tx, mut up_rx) = up.split();
    let (mut cl_tx, mut cl_rx) = client.split();
    let s1 = sess.clone();
    let to_up = async move {
        while let Some(Ok(m)) = cl_rx.next().await {
            s1.last_active.store(now(), Ordering::Relaxed);
            let t = match m { AMsg::Text(t) => TMsg::Text(t), AMsg::Binary(b) => TMsg::Binary(b), AMsg::Ping(p) => TMsg::Ping(p), AMsg::Pong(p) => TMsg::Pong(p), AMsg::Close(_) => break };
            if up_tx.send(t).await.is_err() { break; }
        }
        let _ = up_tx.close().await;
    };
    let to_client = async move {
        while let Some(Ok(m)) = up_rx.next().await {
            let a = match m { TMsg::Text(t) => AMsg::Text(t), TMsg::Binary(b) => AMsg::Binary(b), TMsg::Ping(p) => AMsg::Ping(p), TMsg::Pong(p) => AMsg::Pong(p), TMsg::Close(_) => break, TMsg::Frame(_) => continue };
            if cl_tx.send(a).await.is_err() { break; }
        }
        let _ = cl_tx.close().await;
    };
    tokio::select! { _ = to_up => {}, _ = to_client => {} }
}

pub fn status_json(s: Option<&Session>, ready: bool, why: &str, cluster: &str) -> Value {
    match s {
        None => json!({ "status": "success", "state": "none", "cluster": cluster }),
        Some(s) => json!({ "status": "success", "state": if ready { "ready" } else { "starting" }, "detail": why,
            "url": format!("/shell/{}/", s.sid), "cluster": cluster, "since": s.created }),
    }
}
