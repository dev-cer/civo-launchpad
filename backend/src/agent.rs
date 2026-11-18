//! Mission Agent: berjalan DI cluster peserta (namespace civo-launchpad), memakai ServiceAccount
//! baca-saja (nodes, pods, services, deployments, HPA, PDB — tanpa Secret/ConfigMap).
//! Tidak ada kredensial yang keluar dari cluster: agen membaca keadaan cluster dan mengirimkannya
//! ke Launchpad lewat HTTPS keluar (long-poll). Launchpad membangunkan agen saat peserta klik "Check".
//!
//!   civo-workshop-backend agent      (env: LAUNCHPAD_URL, AGENT_TOKEN)
use serde_json::Value;
use std::time::Duration;

pub const VERSION: &str = "1";
use crate::kube::Kube;

/// Buang field besar yang tidak dipakai penilaian (managedFields, last-applied-configuration).
fn slim(mut v: Value, kind: &str) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("kind".into(), Value::String(kind.into()));
        if let Some(m) = o.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            m.remove("managedFields");
            if let Some(a) = m.get_mut("annotations").and_then(|a| a.as_object_mut()) { a.remove("kubectl.kubernetes.io/last-applied-configuration"); }
        }
    }
    v
}

async fn collect(k: &Kube) -> Value {
    let nodes = match k.get("/api/v1/nodes").await {
        Ok(v) => serde_json::json!({ "items": v["items"].as_array().cloned().unwrap_or_default().into_iter().map(|n| slim(n, "Node")).collect::<Vec<_>>() }),
        Err(e) => return serde_json::json!({ "agent": VERSION, "error": e }),
    };
    let ns = "nusamart";
    let parts = [
        ("Deployment", format!("/apis/apps/v1/namespaces/{ns}/deployments")),
        ("Service", format!("/api/v1/namespaces/{ns}/services")),
        ("HorizontalPodAutoscaler", format!("/apis/autoscaling/v2/namespaces/{ns}/horizontalpodautoscalers")),
        ("PodDisruptionBudget", format!("/apis/policy/v1/namespaces/{ns}/poddisruptionbudgets")),
        ("Pod", format!("/api/v1/namespaces/{ns}/pods")),
    ];
    let mut items = vec![];
    let mut ns_error = String::new();
    for (kind, path) in parts {
        match k.get(&path).await {
            Ok(v) => items.extend(v["items"].as_array().cloned().unwrap_or_default().into_iter().map(|i| slim(i, kind))),
            Err(e) => { ns_error = e; break; }
        }
    }
    serde_json::json!({ "agent": VERSION, "nodes": nodes, "ns": { "items": items }, "ns_error": ns_error })
}

pub async fn run() {
    let url = std::env::var("LAUNCHPAD_URL").unwrap_or_default().trim_end_matches('/').to_string();
    let token = std::env::var("AGENT_TOKEN").unwrap_or_default();
    if url.is_empty() || token.is_empty() { eprintln!("LAUNCHPAD_URL and AGENT_TOKEN are required"); std::process::exit(2); }
    let kube = match Kube::new() { Ok(k) => k, Err(e) => { eprintln!("Mission Agent must run inside Kubernetes: {e}"); std::process::exit(2); } };
    // Long-poll: Launchpad menahan permintaan sampai ~20 detik, atau menjawab segera saat peserta klik Check.
    let http = reqwest::Client::builder().timeout(Duration::from_secs(45)).build().unwrap();
    println!("Civo Launchpad Mission Agent v{VERSION} → {url}");
    let mut fails = 0u32;
    loop {
        let snap = collect(&kube).await;
        if let Some(e) = snap["error"].as_str() { eprintln!("collect: {e}"); }
        match http.post(format!("{url}/api/agent/report")).bearer_auth(&token).json(&snap).send().await {
            Ok(r) if r.status().is_success() => {
                fails = 0;
                let j: Value = r.json().await.unwrap_or_default();
                if j["stop"] == true { println!("Launchpad closed the event — sleeping. Remove me with: kubectl delete ns civo-launchpad"); tokio::time::sleep(Duration::from_secs(3600)).await; }
            }
            Ok(r) => { fails += 1; eprintln!("report: HTTP {}", r.status().as_u16()); if r.status().as_u16() == 401 { tokio::time::sleep(Duration::from_secs(60)).await; } }
            Err(e) => { fails += 1; eprintln!("report: {e}"); }
        }
        if fails > 0 { tokio::time::sleep(Duration::from_secs((2u64 << fails.min(4)).min(30))).await; }
    }
}

/// Manifest yang dipasang peserta dengan satu perintah: kubectl apply -f <launchpad>/join/<token>.yaml
pub fn manifest(launchpad: &str, token: &str, image: &str) -> String {
    use sha2::{Digest, Sha256};
    // Berubah bila token berubah, sehingga pod agen di-restart dan memakai token baru.
    let link: String = Sha256::digest(token.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!(r#"# Civo Launchpad Mission Agent — read-only access for the Beyond the Container mission race.
# It reads nodes, pods, services, deployments, HPAs and PDBs (never Secrets or ConfigMaps) and reports
# them to Launchpad over HTTPS. Remove it any time with:  kubectl delete ns civo-launchpad
apiVersion: v1
kind: Namespace
metadata:
  name: civo-launchpad
  labels: {{ app.kubernetes.io/part-of: civo-launchpad }}
---
apiVersion: v1
kind: ServiceAccount
metadata: {{ name: mission-agent, namespace: civo-launchpad }}
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata: {{ name: civo-launchpad-read }}
rules:
  - apiGroups: [""]
    resources: ["nodes", "pods", "services"]
    verbs: ["get", "list"]
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["get", "list"]
  - apiGroups: ["autoscaling"]
    resources: ["horizontalpodautoscalers"]
    verbs: ["get", "list"]
  - apiGroups: ["policy"]
    resources: ["poddisruptionbudgets"]
    verbs: ["get", "list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: {{ name: civo-launchpad-read }}
roleRef: {{ apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: civo-launchpad-read }}
subjects:
  - {{ kind: ServiceAccount, name: mission-agent, namespace: civo-launchpad }}
---
apiVersion: v1
kind: Secret
metadata: {{ name: mission-agent, namespace: civo-launchpad }}
stringData:
  LAUNCHPAD_URL: "{launchpad}"
  AGENT_TOKEN: "{token}"
---
apiVersion: apps/v1
kind: Deployment
metadata: {{ name: mission-agent, namespace: civo-launchpad, labels: {{ app: mission-agent }} }}
spec:
  replicas: 1
  selector: {{ matchLabels: {{ app: mission-agent }} }}
  template:
    metadata:
      labels: {{ app: mission-agent }}
      annotations: {{ launchpad.civo.id/link: "{link}" }}
    spec:
      serviceAccountName: mission-agent
      securityContext: {{ runAsNonRoot: true, runAsUser: 10001, seccompProfile: {{ type: RuntimeDefault }} }}
      containers:
        - name: agent
          image: {image}
          args: ["agent"]
          envFrom: [{{ secretRef: {{ name: mission-agent }} }}]
          resources:
            requests: {{ cpu: 10m, memory: 16Mi }}
            limits: {{ cpu: 100m, memory: 64Mi }}
          securityContext: {{ allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: {{ drop: ["ALL"] }} }}
"#)
}
