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
    /// email -> missions whose paid hint was opened (each costs HINT_PENALTY_S on the board and 10 points)
    #[serde(default)]
    pub hints: HashMap<String, Vec<String>>,
}

/// Time penalty per opened hint on the leaderboard (seconds). Env HINT_PENALTY_S.
pub fn hint_penalty() -> u64 { std::env::var("HINT_PENALTY_S").ok().and_then(|v| v.parse().ok()).unwrap_or(120) }

/// Paid hint per mission: the full answer, including the planted bug.
pub fn mission_hint(m: &str) -> &'static str {
    match m {
        "M0" => "Civo dashboard → Kubernetes → Create cluster: region Jakarta (JKT1), 3 nodes, g4s.kube.medium, CNI Cilium. When it is Active, download the kubeconfig, open Launchpad → Terminal → Open my shell and drop the file in. kubectl get nodes must list 3 Ready nodes.",
        "M1" => "🐞 Bug: the Service selector is app=ecommerce, but the pods are labelled app=ecommerce-rust — so the load balancer has an IP but no pods behind it. Compare: kubectl get svc ecommerce-rust-svc -n nusamart -o jsonpath='{.spec.selector}' and kubectl get pods -n nusamart --show-labels. Fix in k8s/base/service.yaml (selector app: ecommerce-rust) and re-apply with kubectl apply -k k8s/overlays/before — or kubectl edit svc ecommerce-rust-svc -n nusamart.",
        "M2" => "The 'before' sizing (CPU 50m/100m, memory 32Mi/64Mi) is Burstable and throttled. kubectl set resources deploy/ecommerce-rust -n nusamart --requests=cpu=500m,memory=128Mi --limits=cpu=500m,memory=128Mi, then kubectl scale deploy/ecommerce-rust -n nusamart --replicas=2.",
        "M3" => "🐞 Bug: k8s/missions/m3-hpa.yaml targets a Deployment named ecommerce-rust-app, which does not exist (kubectl describe hpa -n nusamart shows the error). Set scaleTargetRef.name to ecommerce-rust, kubectl apply -f k8s/missions/m3-hpa.yaml, then run the load test: kubectl delete job k6-flash-sale -n loadtest --ignore-not-found; kubectl apply -k k6 — and watch kubectl get hpa -n nusamart -w.",
        "M4" => "🐞 Bug: k8s/missions/m4-pdb.yaml says minAvailable: 100%, so ALLOWED DISRUPTIONS is 0 and kubectl drain waits forever. Change it to minAvailable: 1 and re-apply. Then kubectl get pods -n nusamart -o wide, drain the node that holds two pods (kubectl drain <node> --ignore-daemonsets --delete-emptydir-data) and kubectl uncordon <node> afterwards.",
        "Final" => "🐞 Bug: k8s/missions/final-spread.yaml spreads on topologyKey kubernetes.io/host — no node has that label, so new pods stay Pending. Use kubernetes.io/hostname and patch again: kubectl patch deploy ecommerce-rust -n nusamart --patch-file k8s/missions/final-spread.yaml. Then warm capacity: kubectl patch hpa ecommerce-rust -n nusamart -p '{\"spec\":{\"minReplicas\":3}}' and wait for 3 Ready pods on 3 nodes.",
        _ => "",
    }
}

/// A check result. `clue` is free; `hint` is the precise fix and is only shown after the paid hint is opened.
#[derive(Serialize, Clone)]
pub struct Check { pub name: String, pub ok: bool, pub clue: String, pub hint: String }
fn ck(name: &str, ok: bool, hint: &str) -> Check {
    let clue = if ok { String::new() } else { clue_for(name).to_string() };
    Check { name: name.into(), ok, clue, hint: if ok { String::new() } else { hint.into() } }
}
/// Free clues: point at the symptom, never at the command.
fn clue_for(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.starts_with("3 nodes") { "Your cluster needs 3 nodes in Ready state — and Launchpad must be able to see it (Connect in M0)." }
    else if n.starts_with("namespace") || n.starts_with("deployment") { "NusaMart is not running in the nusamart namespace yet. Start from the repo's 'before' overlay." }
    else if n.starts_with("original") { "The image and RECO_WORK must stay as shipped in the repo." }
    else if n.starts_with("at least 1 pod") { "Look at the pods: are they Running and READY?" }
    else if n.starts_with("service selector") { "The load balancer only sends traffic to pods whose labels match the Service. Do they match?" }
    else if n.starts_with("load balancer") { "A LoadBalancer Service gets its public IP after 1–2 minutes." }
    else if n.starts_with("shop answers") { "The IP exists but nothing answers. Follow the path: load balancer → Service → pods." }
    else if n.starts_with("requests = limits") { "Guaranteed QoS needs requests equal to limits for both CPU and memory." }
    else if n.starts_with("running pods are qos") { "The new pods appear after the rollout — check the QOS class of the running pods." }
    else if n.starts_with("2 or more replicas") { "One pod is a single point of failure." }
    else if n.starts_with("hpa targets") { "Is there an HPA — and does it point at a Deployment that really exists?" }
    else if n.starts_with("hpa has scaled") { "An HPA only scales when CPU is measured and there is load. Check TARGETS, then run the k6 test." }
    else if n.starts_with("pdb protects") { "Nothing limits how many NusaMart pods a drain may evict at once." }
    else if n.starts_with("pdb allows") { "Your budget leaves no room for even one eviction — a drain would wait forever." }
    else if n.starts_with("pods run on 2") { "All your pods sit on one node. What happens when it is drained?" }
    else if n.starts_with("no node left") { "A drained node stays unschedulable until you bring it back." }
    else if n.starts_with("hpa minreplicas") { "Warm capacity: the autoscaler should never go below 3 pods during the sale." }
    else if n.starts_with("3 or more replicas") { "Are all 3 pods Ready — or is one stuck Pending?" }
    else if n.starts_with("spread rule") { "The spread rule must use a label that every node really has." }
    else if n.starts_with("pods spread over 3") { "Three nodes, three pods — one on each. Look at the NODE column." }
    else if n.starts_with("flash sale probe") { "Too slow or errors under the burst: check resources, replicas and the HPA." }
    else { "Check the mission goal again." }
}

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
    let pen = hint_penalty();
    let mut rows: Vec<(usize, u64, String, Vec<(String, u64)>, usize)> = d.done.iter()
        .map(|(email, (name, all))| { let ms = ranked(d, all); let h = d.hints.get(email).map(|v| v.len()).unwrap_or(0);
            (ms.len(), ms.iter().map(|m| m.1).max().unwrap_or(0).saturating_sub(start) + h as u64 * pen, name.clone(), ms, h) })
        .filter(|r| r.0 > 0)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    rows.into_iter().enumerate().map(|(i, (n, t, name, ms, h))| serde_json::json!({
        "rank": i + 1, "name": short_name(&name), "missions": n, "elapsed": t, "hints": h, "score": (n as i64) * 100 - (h as i64) * 10,
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
fn ctr_spread(dep: &serde_json::Value) -> bool {
    dep["spec"]["template"]["spec"]["topologySpreadConstraints"].as_array().map(|a| a.iter().any(|t|
        t["topologyKey"] == "kubernetes.io/hostname" && t["whenUnsatisfiable"] == "DoNotSchedule")).unwrap_or(false)
}

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
    let sel_ok = svc.as_ref().map(|s| s["spec"]["selector"]["app"] == APP).unwrap_or(false);
    c.push(ck("Service selector matches the NusaMart pods", sel_ok, "Set the Service selector to app: ecommerce-rust (k8s/base/service.yaml, then kubectl apply -k k8s/overlays/before)."));
    let ip = svc.as_ref().and_then(|s| s["status"]["loadBalancer"]["ingress"][0]["ip"].as_str().map(String::from));
    c.push(ck("Load balancer has an External IP", ip.is_some(), "kubectl get svc ecommerce-rust-svc -n nusamart -w — wait for EXTERNAL-IP (1–2 min)."));
    let health = match &ip { Some(ip) => matches!(http_get(ip, "/health").await, Ok((200, _))), None => false };
    c.push(ck("Shop answers on http://<EXTERNAL-IP>/health", health, "The load balancer is not answering yet — wait a minute and check again."));
    if idx == 1 { return c; }

    let pods: Vec<_> = items(list, "Pod").into_iter().filter(|p| p["metadata"]["labels"]["app"] == APP && p["status"]["phase"] == "Running").cloned().collect();
    let res = &ctr["resources"];
    let guaranteed_spec = q_eq(&res["requests"]["cpu"], &res["limits"]["cpu"]) && q_eq(&res["requests"]["memory"], &res["limits"]["memory"]);
    let guaranteed_pods = !pods.is_empty() && pods.iter().all(|p| p["status"]["qosClass"] == "Guaranteed");
    c.push(ck("Requests = limits (CPU and memory)", guaranteed_spec, "kubectl set resources … --requests=\"cpu=500m,memory=128Mi\" --limits=\"cpu=500m,memory=128Mi\""));
    c.push(ck("Running pods are QoS Guaranteed", guaranteed_pods, "Wait for the rollout to finish, then check the QOS column."));
    c.push(ck("2 or more replicas ready", ready_rep >= 2, "kubectl scale deploy/ecommerce-rust -n nusamart --replicas=2"));
    if idx == 2 { return c; }

    let hpa = items(list, "HorizontalPodAutoscaler").into_iter().find(|h| h["spec"]["scaleTargetRef"]["name"] == APP).cloned();
    let min = hpa.as_ref().and_then(|h| h["spec"]["minReplicas"].as_u64()).unwrap_or(1);
    let max = hpa.as_ref().and_then(|h| h["spec"]["maxReplicas"].as_u64()).unwrap_or(0);
    c.push(ck("HPA targets ecommerce-rust (min ≥ 2, max ≥ 4)", hpa.is_some() && min >= 2 && max >= 4, "kubectl autoscale deploy ecommerce-rust -n nusamart --cpu-percent=60 --min=2 --max=8"));
    let scaled = hpa.as_ref().map(|h| !h["status"]["lastScaleTime"].is_null()).unwrap_or(false);
    c.push(ck("HPA has scaled at least once under load", scaled, "Wait until kubectl get hpa -n nusamart shows a CPU % under TARGETS (not <unknown>), then run the load test: kubectl delete job k6-flash-sale -n loadtest --ignore-not-found; kubectl apply -k k6 — and watch kubectl get hpa -n nusamart -w until REPLICAS goes up."));
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

    c.push(ck("HPA minReplicas ≥ 3 (warm capacity)", min >= 3, "kubectl delete hpa ecommerce-rust -n nusamart, then: kubectl autoscale deploy ecommerce-rust -n nusamart --cpu-percent=60 --min=3 --max=8"));
    c.push(ck("3 or more replicas ready", ready_rep >= 3, "Wait until 3 pods are Ready."));
    let spread_ok = ctr_spread(&dep);
    c.push(ck("Spread rule on kubernetes.io/hostname (DoNotSchedule)", spread_ok, "Use topologyKey: kubernetes.io/hostname with whenUnsatisfiable: DoNotSchedule in k8s/missions/final-spread.yaml."));
    c.push(ck("Pods spread over 3 nodes", on_nodes.len() >= 3, "Add topologySpreadConstraints on kubernetes.io/hostname with whenUnsatisfiable: DoNotSchedule (ScheduleAnyway is only a preference) — see bonus B2, then wait for the rollout."));
    let (failed, p95) = match &ip { Some(ip) => burst(ip).await, None => (120, 0) };
    let limit: u128 = std::env::var("RACE_P95_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    c.push(ck(&format!("Flash sale probe: 120 requests, {failed} failed, p95 {p95} ms (limit {limit} ms)"), failed == 0 && p95 <= limit, "Too slow or errors under burst: check resources, replicas and HPA."));
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snap(sel: &str, hpa_target: &str, pdb_min_allowed: u64, key: &str) -> Snapshot {
        let node = |n: &str| serde_json::json!({"metadata":{"name":n},"spec":{},"status":{"conditions":[{"type":"Ready","status":"True"}]}});
        let pod = |n: &str, node: &str| serde_json::json!({"kind":"Pod","metadata":{"name":n,"labels":{"app":"ecommerce-rust"}},"spec":{"nodeName":node},"status":{"phase":"Running","qosClass":"Guaranteed"}});
        let res = serde_json::json!({"requests":{"cpu":"500m","memory":"128Mi"},"limits":{"cpu":"500m","memory":"128Mi"}});
        Snapshot {
            nodes: serde_json::json!({"items":[node("a"),node("b"),node("c")]}),
            ns: Ok(serde_json::json!({"items":[
                {"kind":"Deployment","metadata":{"name":"ecommerce-rust"},"status":{"readyReplicas":3},"spec":{"template":{"spec":{
                    "topologySpreadConstraints":[{"topologyKey":key,"whenUnsatisfiable":"DoNotSchedule"}],
                    "containers":[{"image":"ghcr.io/dev-cer/ecommerce-app:1","env":[{"name":"RECO_WORK","value":"100000"}],"resources":res}]}}}},
                {"kind":"Service","metadata":{"name":"ecommerce-rust-svc"},"spec":{"selector":{"app":sel}},"status":{"loadBalancer":{}}},
                {"kind":"HorizontalPodAutoscaler","metadata":{"name":"ecommerce-rust"},"spec":{"scaleTargetRef":{"name":hpa_target},"minReplicas":3,"maxReplicas":8},"status":{"lastScaleTime":"x"}},
                {"kind":"PodDisruptionBudget","metadata":{"name":"ecommerce-rust"},"spec":{"selector":{"matchLabels":{"app":"ecommerce-rust"}}},"status":{"disruptionsAllowed":pdb_min_allowed}},
                pod("p1","a"),pod("p2","b"),pod("p3","c")]})),
        }
    }
    fn failing(s: &Snapshot, m: &str) -> Vec<String> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(evaluate(s, m)).into_iter().filter(|c| !c.ok).map(|c| { assert!(!c.clue.is_empty()); c.name }).collect()
    }
    #[test]
    fn planted_bugs_are_caught_with_a_clue() {
        assert!(failing(&snap("ecommerce", "ecommerce-rust", 1, "kubernetes.io/hostname"), "M1").iter().any(|n| n.starts_with("Service selector")));
        assert!(failing(&snap("ecommerce-rust", "ecommerce-rust-app", 1, "kubernetes.io/hostname"), "M3").iter().any(|n| n.starts_with("HPA targets")));
        assert!(failing(&snap("ecommerce-rust", "ecommerce-rust", 0, "kubernetes.io/hostname"), "M4").iter().any(|n| n.starts_with("PDB allows")));
        assert!(failing(&snap("ecommerce-rust", "ecommerce-rust", 1, "kubernetes.io/host"), "Final").iter().any(|n| n.starts_with("Spread rule")));
        let ok = failing(&snap("ecommerce-rust", "ecommerce-rust", 1, "kubernetes.io/hostname"), "M4");
        assert!(!ok.iter().any(|n| n.starts_with("Service selector") || n.starts_with("HPA targets") || n.starts_with("PDB")), "{ok:?}");
        for m in MISSIONS { assert!(!mission_hint(m).is_empty()); }
    }
}
