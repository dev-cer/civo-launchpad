//! Klien Kubernetes API in-cluster yang ringan (dipakai Mission Agent dan Launchpad Shell).
//! Override untuk uji lokal: KUBE_API, KUBE_TOKEN_FILE, KUBE_CA_FILE.
use serde_json::Value;
use std::time::Duration;

const SA: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

pub struct Kube { client: reqwest::Client, api: String, token_file: String }

impl Kube {
    pub fn new() -> Result<Kube, String> {
        let ca_file = std::env::var("KUBE_CA_FILE").unwrap_or_else(|_| format!("{SA}/ca.crt"));
        let ca = std::fs::read(&ca_file).map_err(|e| format!("no service account CA ({ca_file}): {e}"))?;
        let cert = reqwest::Certificate::from_pem(&ca).map_err(|e| format!("bad CA: {e}"))?;
        let client = reqwest::Client::builder().add_root_certificate(cert).timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
        Ok(Kube {
            client,
            api: std::env::var("KUBE_API").unwrap_or_else(|_| "https://kubernetes.default.svc".into()).trim_end_matches('/').to_string(),
            token_file: std::env::var("KUBE_TOKEN_FILE").unwrap_or_else(|_| format!("{SA}/token")),
        })
    }
    fn token(&self) -> Result<String, String> {
        // Token ServiceAccount diputar otomatis oleh kubelet: baca ulang setiap kali.
        Ok(std::fs::read_to_string(&self.token_file).map_err(|e| format!("no service account token: {e}"))?.trim().to_string())
    }
    async fn send(&self, rb: reqwest::RequestBuilder, path: &str) -> Result<(u16, Value), String> {
        let r = rb.bearer_auth(self.token()?).send().await.map_err(|e| format!("API server: {e}"))?;
        let code = r.status().as_u16();
        let v = r.json::<Value>().await.unwrap_or(Value::Null);
        if code >= 400 && code != 404 && code != 409 {
            return Err(format!("{path}: HTTP {code} {}", v["message"].as_str().unwrap_or("")));
        }
        Ok((code, v))
    }
    pub async fn get(&self, path: &str) -> Result<Value, String> {
        let (code, v) = self.send(self.client.get(format!("{}{path}", self.api)), path).await?;
        if code == 404 { return Err(format!("{path}: HTTP 404")); }
        Ok(v)
    }
    /// GET yang membedakan "tidak ada" (Ok(None)) dari error.
    pub async fn get_opt(&self, path: &str) -> Result<Option<Value>, String> {
        let (code, v) = self.send(self.client.get(format!("{}{path}", self.api)), path).await?;
        Ok(if code == 404 { None } else { Some(v) })
    }
    pub async fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        self.send(self.client.post(format!("{}{path}", self.api)).json(body), path).await
    }
    pub async fn delete(&self, path: &str) -> Result<u16, String> {
        let (code, _) = self.send(self.client.delete(format!("{}{path}", self.api)).json(&serde_json::json!({"gracePeriodSeconds": 2})), path).await?;
        Ok(code)
    }
}
