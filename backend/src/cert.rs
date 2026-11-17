//! Sertifikat otomatis "Beyond the Container" dengan QR verifikasi (pengganti tanda tangan basah).
//!
//!   POST /api/cert/me   {email, token}  -> {certificate, released, id, url, verify_url}
//!   GET  /cert/:id      -> sertifikat A4 landscape (cetak / Save as PDF dari browser)
//!   GET  /verify/:id    -> halaman verifikasi publik (tujuan QR code)
//!
//! Credential ID = BTC-2026-XXXX-XXXX, diturunkan dari SHA-256(CERT_SECRET | email). Tidak bisa
//! ditebak tanpa CERT_SECRET, dan tetap sama walaupun peserta naik dari Participation ke Completion.
//! Sertifikat terbit setelah race di-stop (atau setelah jendela event ditutup).
use qrcode::{render::svg, EcLevel, QrCode};
use sha2::{Digest, Sha256};

pub const LOGO: &str = include_str!("civo-logo.svg");

pub fn cert_id(secret: &str, email: &str) -> String {
    let h = Sha256::digest(format!("btc-2026|{secret}|{}", email.trim().to_lowercase()).as_bytes());
    let x: String = h.iter().take(4).map(|b| format!("{b:02X}")).collect();
    format!("BTC-2026-{}-{}", &x[..4], &x[4..])
}

pub fn normalize_id(id: &str) -> String {
    id.trim().to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(24).collect()
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

pub fn qr_svg(url: &str, px: u32) -> String {
    match QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M) {
        Ok(c) => c.render::<svg::Color>().min_dimensions(px, px).quiet_zone(false)
            .dark_color(svg::Color("#000000")).light_color(svg::Color("#ffffff")).build()
            .replacen("<?xml version=\"1.0\" standalone=\"yes\"?>", "", 1),
        Err(_) => String::new(),
    }
}

const MISSION_NAMES: [(&str, &str); 6] = [("M0", "Launch"), ("M1", "Deploy &amp; Expose"), ("M2", "Right-size"),
    ("M3", "Scale"), ("M4", "Stay Available"), ("Final", "Flash Sale Ready")];

fn chip(id: &str, name: &str) -> String {
    format!(r##"<div class="chip"><svg aria-hidden="true" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="#00804a" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg><span>{id} {name}</span></div>"##)
}

const HEAD: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Newsreader:opsz,wght@6..72,400..700&family=Hanken+Grotesk:wght@400..800&family=JetBrains+Mono:wght@500;700&display=swap" rel="stylesheet">"#;

/// Sertifikat lengkap. `kind` = "Completion" | "Participation".
pub fn page(kind: &str, name: &str, id: &str, verify_url: &str) -> String {
    let completion = kind == "Completion";
    let chips: String = MISSION_NAMES.iter().take(if completion { 6 } else { 3 }).map(|(i, n)| chip(i, n)).collect();
    let (label, body, big, small, panel_bg, panel_fg) = if completion {
        ("CERTIFICATE OF COMPLETION",
         "has completed all six missions of the hands-on workshop, verified live on their own Civo Kubernetes cluster in the Jakarta region (JKT1).",
         "6/6", "missions verified on a live cluster", "#00ff6f", "#000000")
    } else {
        ("CERTIFICATE OF PARTICIPATION",
         "has taken part in the hands-on workshop and deployed, right-sized and operated an application on their own Civo Kubernetes cluster in the Jakarta region (JKT1).",
         "Hands-on", "workshop participant", "#0b0b0b", "#ffffff")
    };
    let big_size = if completion { 112 } else { 54 };
    let logo = LOGO.replacen("<svg ", r#"<svg class="logo" "#, 1);
    let qr = qr_svg(verify_url, 92);
    let title = format!("Certificate of {kind} — {}", esc(name));
    let short_url = verify_url.trim_start_matches("https://").trim_start_matches("http://");
    format!(r##"{HEAD}<title>{title}</title>
<style>
@page{{size:A4 landscape;margin:0}}
*{{box-sizing:border-box}} body{{margin:0;background:#e9ece9;font-family:'Hanken Grotesk',Arial,sans-serif;color:#0d0d0d}}
.bar{{display:flex;gap:10px;justify-content:center;align-items:center;padding:14px;flex-wrap:wrap;font-size:14px}}
.bar button,.bar a{{font:inherit;font-weight:700;border:1.5px solid #000;border-radius:999px;padding:9px 18px;background:#fff;color:#000;cursor:pointer;text-decoration:none}}
.bar button{{background:#00ff6f}}
.wrap{{display:grid;place-items:center;padding:0 12px 28px}}
.sheet{{width:1123px;height:794px;display:flex;background:#fff;box-shadow:0 20px 60px rgba(0,0,0,.18);transform-origin:top center}}
.main{{flex:1;min-width:0;padding:56px 56px 48px 64px;display:flex;flex-direction:column;justify-content:space-between}}
.top{{display:flex;flex-direction:column;gap:26px}}
.hdr{{display:flex;justify-content:space-between;align-items:center}}
.logo{{width:118px;height:auto}}
.lbl{{margin:0;font-size:14px;font-weight:700;letter-spacing:.14em;color:#00804a}}
h1{{margin:0;font-family:'Newsreader',Georgia,serif;font-size:{name_size}px;font-weight:500;line-height:1.05;letter-spacing:-.01em}}
.p{{margin:0;font-size:18px;color:#3d3d3d}}
.chips{{display:flex;flex-wrap:wrap;gap:8px;max-width:650px}}
.chip{{display:flex;align-items:center;gap:8px;padding:8px 14px;border-radius:999px;background:#edfff4;border:1.5px solid #00804a;font-size:14px;font-weight:700;color:#0b3d24}}
.foot{{display:flex;align-items:flex-end;gap:40px}}
.sig{{display:flex;gap:16px;align-items:flex-end}}
.qr svg{{display:block;width:92px;height:92px}}
.k{{margin:0;font-size:12px;font-weight:700;letter-spacing:.12em;color:#6b6b6b}}
.v{{margin:0;font-size:15px;font-weight:600}}
.mono{{font-family:'JetBrains Mono',Consolas,monospace}}
.panel{{width:300px;flex:none;background:{panel_bg};color:{panel_fg};padding:56px 40px 48px;display:flex;flex-direction:column;justify-content:space-between}}
@media print{{html,body{{width:1123px;height:794px;overflow:hidden;background:#fff}} .bar{{display:none}} .wrap{{padding:0;height:auto!important;display:block}} .sheet{{page-break-after:avoid}} .sheet{{box-shadow:none;transform:none!important}}
  *{{-webkit-print-color-adjust:exact;print-color-adjust:exact}}}}
</style></head><body>
<div class="bar"><button onclick="window.print()">⬇ Download PDF</button><a href="{verify_url}" target="_blank" rel="noopener">✓ Verification page</a>
<a href="https://www.linkedin.com/profile/add?startTask=CERTIFICATION_NAME&name={li_name}&organizationName=Civo%20Indonesia&issueYear=2026&issueMonth=11&certId={id}&certUrl={li_url}" target="_blank" rel="noopener">in Add to LinkedIn</a></div>
<div class="wrap"><div class="sheet" id="sheet">
<div class="main"><div class="top">
  <div class="hdr">{logo}<p class="lbl">{label}</p></div>
  <div style="display:flex;flex-direction:column;gap:10px"><p class="p">This certifies that</p><h1>{name_h}</h1>
    <p class="p" style="margin-top:6px;line-height:1.5;color:#2b2b2b;max-width:600px">{body}</p></div>
  <div style="display:flex;flex-direction:column;gap:6px"><p style="margin:0;font-family:'Newsreader',Georgia,serif;font-size:30px;font-weight:600;line-height:1.1">Beyond the Container</p>
    <p style="margin:0;font-size:16px;color:#3d3d3d">Running Highly Available &amp; Scalable Applications with Kubernetes</p></div>
  <div class="chips">{chips}</div>
</div>
<div class="foot">
  <div class="sig"><div class="qr">{qr}</div>
    <div style="display:flex;flex-direction:column;gap:3px;width:230px">
      <p class="k" style="color:#00804a">DIGITALLY SIGNED</p>
      <p style="margin:0;font-size:16px;font-weight:700">Ancer Afriono</p>
      <p style="margin:0;font-size:13px;color:#4a4a4a;line-height:1.35">Product Manager, Civo Indonesia<br>PT Arupa Cloud Nusantara</p>
      <p style="margin:4px 0 0;font-size:11px;color:#6b6b6b">Scan to verify</p></div></div>
  <div style="display:flex;flex-direction:column;gap:4px">
    <p class="k">ISSUED</p><p class="v">26 November 2026, Jakarta</p>
    <p class="k" style="margin-top:10px">CERTIFICATE ID</p><p class="v mono">{id}</p></div>
</div></div>
<div class="panel"><p style="margin:0;font-size:13px;font-weight:700;letter-spacing:.14em">CIVO WORKSHOP · JAKARTA</p>
  <div style="display:flex;flex-direction:column;gap:10px"><p style="margin:0;font-family:'Newsreader',Georgia,serif;font-size:{big_size}px;font-weight:500;line-height:.95;white-space:nowrap">{big}</p>
    <p style="margin:0;font-size:17px;line-height:1.35;font-weight:600">{small}</p></div>
  <div style="display:flex;flex-direction:column;gap:6px;font-size:14px;line-height:1.4"><p style="margin:0;font-weight:700">Thursday, 26 November 2026</p>
    <p style="margin:0">Civo Kubernetes · Jakarta region (JKT1)</p><p style="margin:0">Civo Indonesia · PT Arupa Cloud Nusantara</p>
    <p class="mono" style="margin:8px 0 0;font-size:11px;opacity:.75;word-break:break-all">{short_url}</p></div></div>
</div></div>
<script>function fit(){{var s=document.getElementById('sheet'),w=Math.min(1,(innerWidth-24)/1123);s.style.transform='scale('+w+')';s.parentNode.style.height=(794*w+28)+'px'}}addEventListener('resize',fit);fit();
addEventListener('beforeprint',function(){{document.getElementById('sheet').style.transform='none'}});addEventListener('afterprint',fit);</script>
</body></html>"##,
        name_size = if name.chars().count() > 26 { 46 } else { 60 },
        name_h = esc(name), li_name = urlenc(&format!("Certificate of {kind} — Beyond the Container (Civo Kubernetes)")),
        li_url = urlenc(verify_url))
}

/// Halaman verifikasi publik (tujuan QR). `found` = None berarti ID tidak dikenal / belum terbit.
pub fn verify_page(id: &str, found: Option<(&str, &str, usize)>, cert_url: &str) -> String {
    let logo = LOGO.replacen("<svg ", r#"<svg style="width:96px;height:auto;display:block" "#, 1);
    let inner = match found {
        Some((kind, name, missions)) => format!(r##"<div class="ok">✓ Valid certificate</div>
<h1>{name}</h1><p>holds the <b>Certificate of {kind}</b> for the hands-on workshop <b>Beyond the Container — Running Highly Available &amp; Scalable Applications with Kubernetes</b>.</p>
<table><tr><td>Certificate ID</td><td class="mono">{id}</td></tr><tr><td>Missions verified</td><td>{missions} of 6, checked live on the holder's own Civo Kubernetes cluster (JKT1)</td></tr>
<tr><td>Issued</td><td>26 November 2026, Jakarta</td></tr><tr><td>Issuer</td><td>Civo Indonesia · PT Arupa Cloud Nusantara</td></tr><tr><td>Signed by</td><td>Ancer Afriono, Product Manager, Civo Indonesia</td></tr></table>
<p><a class="btn" href="{cert_url}">View certificate</a></p>"##, name = esc(name), id = esc(id)),
        None => format!(r##"<div class="no">✕ Not found</div><h1>No certificate with this ID</h1>
<p>We could not find a released certificate with ID <span class="mono">{}</span>. Check the ID on the certificate, or contact Civo Indonesia.</p>"##, esc(id)),
    };
    format!(r##"{HEAD}<title>Verify certificate {id_e} — Civo Indonesia</title>
<style>body{{margin:0;background:#f4f6f4;font-family:'Hanken Grotesk',Arial,sans-serif;color:#0d0d0d}} .c{{max-width:680px;margin:40px auto;background:#fff;border-radius:18px;padding:36px 40px;box-shadow:0 10px 40px rgba(0,0,0,.08)}}
h1{{font-family:'Newsreader',Georgia,serif;font-weight:500;font-size:40px;margin:14px 0 8px}} p{{font-size:17px;line-height:1.5;color:#2b2b2b}}
.ok,.no{{display:inline-block;margin-top:22px;font-weight:800;border-radius:999px;padding:6px 14px}} .ok{{background:#00ff6f}} .no{{background:#ffd9d9;color:#8a0000}}
table{{border-collapse:collapse;width:100%;font-size:15px;margin:16px 0}} td{{padding:9px 0;border-bottom:1px solid #e4e4e4;vertical-align:top}} td:first-child{{color:#6b6b6b;width:38%}}
.mono{{font-family:'JetBrains Mono',Consolas,monospace}} .btn{{display:inline-block;background:#000;color:#fff;border-radius:999px;padding:10px 20px;font-weight:700;text-decoration:none}}</style></head>
<body><div class="c">{logo}{inner}</div></body></html>"##, id_e = esc(id))
}

fn urlenc(s: &str) -> String {
    s.bytes().map(|b| match b { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(), _ => format!("%{b:02X}") }).collect()
}
