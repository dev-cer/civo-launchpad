//! Race mode (workshop 26 Nov "Beyond the Container"): siapa paling cepat menyelesaikan misi.
//! Setiap misi diverifikasi SERVER-SIDE ke cluster peserta (kubeconfig dari Secret workshop-kubeconfigs)
//! memakai `kubectl` + HTTP probe ke External IP. Peserta tidak bisa "mengklaim" misi sendiri.
//!
//!   POST /api/race/check   {email, access_code, mission}  -> cek misi, catat waktu lulus pertama
//!   GET  /api/race         -> status race + leaderboard (urut: misi terbanyak, lalu waktu tercepat)
//!   POST /api/admin/race   {action:"start"|"stop"|"reset"}  (Authorization: Bearer $ADMIN_TOKEN)
//!
//! Env: KUBECTL (default "kubectl"), RACE_FILE (default /data/race.json), RACE_P95_MS (default 1500),
//!      RACE_START (opsional, RFC3339: mulai otomatis pada waktu itu).
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub const MISSIONS: [&str; 6] = ["M0", "M1", "M2", "M3", "M4", "Final"];
pub const NS: &str = "nusamart";
pub const APP: &str = "ecommerce-rust";
pub const SVC: &str = "ecommerce-rust-svc";

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct RaceData {
    pub start: Option<u64>,
    pub stop: Option<u64>,
    /// email -> (nama, [(misi, unix detik lulus)])
    pub done: HashMap<String, (String, Vec<(String, u64)>)>,
}

#[derive(Serialize, Clone)]
pub struct Check { pub name: String, pub ok: bool, pub hint: String }
fn ck(name: &str, ok: bool, hint: &str) -> Check { Check { name: name.into(), ok, hint: if ok { String::new() } else { hint.into() } } }

pub fn load(path: &str) -> RaceData {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
pub fn save(path: &str, d: &RaceData) {
    if let Ok(s) = serde_json::to_string(d) { let _ = std::fs::write(path, s); }
}

pub fn short_name(n: &str) -> String {
    let mut it = n.split_whitespace();
    let first = it.next().unwrap_or("?");
    match it.last() { Some(l) => format!("{first} {}.", l.chars().next().unwrap_or(' ').to_uppercase()), None => first.to_string() }
}

/// Sertifikat: Completion = M0..Final lulus (sampai event ditutup), Participation = M0..M2 lulus.
pub fn certificate(passed: &[(String, u64)]) -> &'static str {
    let has = |m: &str| passed.iter().any(|x| x.0 == m);
    if MISSIONS.iter().all(|m| has(m)) { "Completion" } else if ["M0", "M1", "M2"].iter().all(|m| has(m)) { "Participation" } else { "" }
}

/// Lulus misi yang dihitung untuk leaderboard: hanya sampai race ditutup (stop). Setelah itu tetap
/// tercatat untuk sertifikat, tetapi tidak mengubah peringkat.
pub fn ranked(d: &RaceData, ms: &[(String, u64)]) -> Vec<(String, u64)> {
    ms.iter().filter(|m| d.stop.map(|s| m.1 <= s).unwrap_or(true)).cloned().collect()
}

/// Leaderboard: misi terbanyak, lalu waktu (detik sejak start) misi terakhir yang lulus, lalu nama.
pub fn board(d: &RaceData) -> Vec<serde_json::Value> {
    let start = d.start.unwrap_or(0);
    let mut rows: Vec<(usize, u64, String, Vec<(String, u64)>)> = d.done.values()
        .map(|(name, all)| { let ms = ranked(d, all); (ms.len(), ms.iter().map(|m| m.1).max().unwrap_or(0).saturating_sub(start), name.clone(), ms) })
        .filter(|r| r.0 > 0)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    rows.into_iter().enumerate().map(|(i, (n, t, name, ms))| serde_json::json!({
        "rank": i + 1, "name": short_name(&name), "missions": n, "elapsed": t,
        "finished": n == MISSIONS.len(),
        "times": ms.iter().map(|(m, at)| (m.clone(), at.saturating_sub(start))).collect::<HashMap<_, _>>()
    })).collect()
}

async fn kubectl(kubeconfig: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    let bin = std::env::var("KUBECTL").unwrap_or_else(|_| "kubectl".into());
    let mut cmd = Command::new(bin);
    cmd.arg("--kubeconfig").arg(kubeconfig).arg("--request-timeout=10s").args(args).arg("-o").arg("json");
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await
        .map_err(|_| "Your cluster did not answer in time.".to_string())?
        .map_err(|e| format!("kubectl not available on the server: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        return Err(if e.contains("NotFound") || e.contains("not found") { "not found".into() } else { e.lines().next().unwrap_or("kubectl error").chars().take(160).collect() });
    }
    serde_json::from_slice(&out.stdout).map_err(|_| "Could not read the cluster response.".into())
}

/// HTTP GET sederhana tanpa dependency (port 80). Mengembalikan (status, durasi ms).
async fn http_get(ip: &str, path: &str) -> Result<(u16, u128), ()> {
    let t = Instant::now();
    let fut = async {
        let mut s = tokio::net::TcpStream::connect((ip, 80)).await.map_err(|_| ())?;
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {ip}\r\nUser-Agent: hackspace-race\r\nConnection: close\r\n\r\n").as_bytes()).await.map_err(|_| ())?;
        let mut buf = Vec::with_capacity(4096);
        s.read_to_end(&mut buf).await.map_err(|_| ())?;
        let line = String::from_utf8_lossy(&buf[..buf.len().min(64)]).to_string();
        line.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).ok_or(())
    };
    let code = tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| ())??;
    Ok((code, t.elapsed().as_millis()))
}

/// 120 request, 12 paralel, ke "/" (halaman toko, memakai CPU). Lulus: 0 gagal dan p95 < RACE_P95_MS.
async fn burst(ip: &str) -> (usize, u128) {
    let mut lat: Vec<u128> = vec![];
    let mut failed = 0usize;
    for _ in 0..10 {
        let futs: Vec<_> = (0..12).map(|_| { let ip = ip.to_string(); tokio::spawn(async move { http_get(&ip, "/").await }) }).collect();
        for f in futs {
            match f.await { Ok(Ok((c, ms))) if c < 400 => lat.push(ms), Ok(Ok((_, ms))) => { failed += 1; lat.push(ms) }, _ => { failed += 1; lat.push(5000) } }
        }
    }
    lat.sort();
    let p95 = lat.get((lat.len() * 95 / 100).saturating_sub(1)).copied().unwrap_or(0);
    (failed, p95)
}

fn items<'a>(list: &'a serde_json::Value, kind: &str) -> Vec<&'a serde_json::Value> {
    list["items"].as_array().map(|a| a.iter().filter(|i| i["kind"] == kind).collect()).unwrap_or_default()
}
fn q_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool { !a.is_null() && a == b }

/// Keadaan cluster peserta yang dibutuhkan untuk menilai misi. Diambil Launchpad lewat kubeconfig,
/// atau dilaporkan Mission Agent yang berjalan di cluster peserta (akses baca-saja).
#[derive(Clone)]
pub struct Snapshot {
    /// List Node (`items`)
    pub nodes: serde_json::Value,
    /// List gabungan Deployment, Service, HPA, PDB, Pod di namespace nusamart (`items[].kind` terisi)
    pub ns: Result<serde_json::Value, String>,
}

/// Ambil snapshot lewat kubeconfig (mode lama: kubeconfig diunggah / kursi cadangan).
pub async fn fetch(kubeconfig: &str) -> Result<Snapshot, String> {
    let nodes = kubectl(kubeconfig, &["get", "nodes"]).await?;
    let ns = kubectl(kubeconfig, &["-n", NS, "get", "deploy,svc,hpa,pdb,pods"]).await;
    Ok(Snapshot { nodes, ns })
}

/// Verifikasi satu misi lewat kubeconfig.
pub async fn verify(kubeconfig: &str, mission: &str) -> Vec<Check> {
    match fetch(kubeconfig).await {
        Ok(s) => evaluate(&s, mission).await,
        Err(e) => vec![ck("Reach your cluster", false, &format!("Launchpad could not reach your cluster: {e}"))],
    }
}

/// Nilai satu misi dari snapshot. Misi bersifat kumulatif: M2 juga butuh kondisi M1, dst.
/// Probe HTTP dan uji beban Final selalu dijalankan dari Launchpad ke External IP (tidak bisa dipalsukan dari cluster).
pub async fn evaluate(snap: &Snapshot, mission: &str) -> Vec<Check> {
    let mut c = vec![];
    let idx = MISSIONS.iter().position(|m| *m == mission).unwrap_or(0);
    let nodes = &snap.nodes;
    let ns = nodes["items"].as_array().cloned().unwrap_or_default();
    let ready = ns.iter().filter(|n| n["status"]["conditions"].as_array().map(|cs| cs.iter().any(|x| x["type"] == "Ready" && x["status"] == "True")).unwrap_or(false)).count();
    let cordoned = ns.iter().filter(|n| n["spec"]["unschedulable"] == true).count();
    c.push(ck("3 nodes Ready", ready >= 3, "kubectl get nodes should show 3 Ready nodes. Ask a mentor if not."));
    if idx == 0 { return c; }

    let list = match &snap.ns {
        Ok(v) => v,
        Err(e) => { c.push(ck("Namespace nusamart", false, &format!("Deploy NusaMart first (kubectl apply -k k8s/overlays/before): {e}"))); return c; }
    };
    let dep = items(list, "Deployment").into_iter().find(|d| d["metadata"]["name"] == APP).cloned();
    let Some(dep) = dep else { c.push(ck("Deployment ecommerce-rust", false, "Run: kubectl apply -k k8s/overlays/before")); return c; };
    let ready_rep = dep["status"]["readyReplicas"].as_u64().unwrap_or(0);
    let ctr = &dep["spec"]["template"]["spec"]["containers"][0];
    let image_ok = ctr["image"].as_str().unwrap_or("").contains("ecommerce-app");
    let reco_ok = ctr["env"].as_array().map(|e| e.iter().any(|x| x["name"] == "RECO_WORK" && x["value"] == "100000")).unwrap_or(false);
    c.push(ck("Original NusaMart image and RECO_WORK", image_ok && reco_ok, "Do not change the image or RECO_WORK — re-apply k8s/overlays/before."));
    c.push(ck("At least 1 pod ready", ready_rep >= 1, "Wait for the pod to be Running and 1/1 READY (kubectl get pods -n nusamart)."));
    let svc = items(list, "Service").into_iter().find(|s| s["metadata"]["name"] == SVC).cloned();
    let ip = svc.as_ref().and_then(|s| s["status"]["loadBalancer"]["ingress"][0]["ip"].as_str().map(String::from));
    c.push(ck("Load balancer has an External IP", ip.is_some(), "kubectl get svc ecommerce-rust-svc -n nusamart -w — wait for EXTERNAL-IP (1–2 min)."));
    let health = match &ip { Some(ip) => matches!(http_get(ip, "/health").await, Ok((200, _))), None => false };
    c.push(ck("Shop answers on http://<EXTERNAL-IP>/health", health, "The load balancer is not answering yet — wait a minute and check again."));
    if idx == 1 { return c; }

    let pods: Vec<_> = items(list, "Pod").into_iter().filter(|p| p["metadata"]["labels"]["app"] == APP && p["status"]["phase"] == "Running").cloned().collect();
    let res = &ctr["resources"];
    let guaranteed_spec = q_eq(&res["requests"]["cpu"], &res["limits"]["cpu"]) && q_eq(&res["requests"]["memory"], &res["limits"]["memory"]);
    let guaranteed_pods = !pods.is_empty() && pods.iter().all(|p| p["status"]["qosClass"] == "Guaranteed");
    c.push(ck("Requests = limits (CPU and memory)", guaranteed_spec, "kubectl set resources … --requests=cpu=500m,memory=128Mi --limits=cpu=500m,memory=128Mi"));
    c.push(ck("Running pods are QoS Guaranteed", guaranteed_pods, "Wait for the rollout to finish, then check the QOS column."));
    c.push(ck("2 or more replicas ready", ready_rep >= 2, "kubectl scale deploy/ecommerce-rust -n nusamart --replicas=2"));
    if idx == 2 { return c; }

    let hpa = items(list, "HorizontalPodAutoscaler").into_iter().find(|h| h["spec"]["scaleTargetRef"]["name"] == APP).cloned();
    let min = hpa.as_ref().and_then(|h| h["spec"]["minReplicas"].as_u64()).unwrap_or(1);
    let max = hpa.as_ref().and_then(|h| h["spec"]["maxReplicas"].as_u64()).unwrap_or(0);
    c.push(ck("HPA targets ecommerce-rust (min ≥ 2, max ≥ 4)", hpa.is_some() && min >= 2 && max >= 4, "kubectl autoscale deploy ecommerce-rust -n nusamart --cpu-percent=60 --min=2 --max=8"));
    let scaled = hpa.as_ref().map(|h| !h["status"]["lastScaleTime"].is_null()).unwrap_or(false);
    c.push(ck("HPA has scaled at least once under load", scaled, "Run the k6 load test (kubectl apply -k k6) and watch kubectl get hpa -n nusamart -w."));
    if idx == 3 { return c; }

    let pdb = items(list, "PodDisruptionBudget").into_iter().find(|p| p["spec"]["selector"]["matchLabels"]["app"] == APP).cloned();
    let allowed = pdb.as_ref().and_then(|p| p["status"]["disruptionsAllowed"].as_u64()).unwrap_or(0);
    c.push(ck("PDB protects ecommerce-rust", pdb.is_some(), "kubectl create pdb ecommerce-rust -n nusamart --selector=app=ecommerce-rust --min-available=1"));
    c.push(ck("PDB allows a safe drain (disruptionsAllowed ≥ 1)", allowed >= 1, "Keep at least 2 ready replicas so one can be evicted safely."));
    let mut on_nodes: Vec<&str> = pods.iter().filter_map(|p| p["spec"]["nodeName"].as_str()).collect();
    on_nodes.sort(); on_nodes.dedup();
    c.push(ck("Pods run on 2 or more nodes", on_nodes.len() >= 2, "Drain the node that holds both pods, or add a topologySpreadConstraint."));
    c.push(ck("No node left cordoned", cordoned == 0, "kubectl uncordon <NODE> after the drain."));
    if idx == 4 { return c; }

    c.push(ck("HPA minReplicas ≥ 3 (warm capacity)", min >= 3, "kubectl patch hpa ecommerce-rust -n nusamart -p '{\"spec\":{\"minReplicas\":3,\"maxReplicas\":8}}'"));
    c.push(ck("3 or more replicas ready", ready_rep >= 3, "Wait until 3 pods are Ready."));
    c.push(ck("Pods spread over 3 nodes", on_nodes.len() >= 3, "Add topologySpreadConstraints (kubernetes.io/hostname) — see bonus B2."));
    let (failed, p95) = match &ip { Some(ip) => burst(ip).await, None => (120, 0) };
    let limit: u128 = std::env::var("RACE_P95_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    c.push(ck(&format!("Flash sale probe: 120 requests, {failed} failed, p95 {p95} ms (limit {limit} ms)"), failed == 0 && p95 <= limit, "Too slow or errors under burst: check resources, replicas and HPA."));
    c
}
