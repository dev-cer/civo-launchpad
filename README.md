# Civo Launchpad

Workshop platform for Civo Indonesia (PT Arupa Cloud Nusantara): labs with a simulated terminal, registration,
**Connect my cluster**, the mission race and **Mission Control** for "Beyond the Container" (26 Nov 2026).

- Image: `ghcr.io/dev-cer/civo-launchpad:workshop-latest` / `:academy-latest`, built by
  `.github/workflows/launchpad-image.yml` from `Dockerfile.launchpad` (Rust API + UI + kubectl).
- Deploy: `deploy/launchpad.yaml`, rendered by `scripts/deploy-launchpad.ps1` in the facilitator kit (nusamart-1212).
- Editions: `python3 tools/build_editions.py` writes `dist/{webinar,workshop,academy}/index.html`.

---

# Civo Kubernetes Workshop — Launchpad

Platform workshop hands-on (gaya KodeKloud / EKS Workshop) untuk Civo Kubernetes.
Dipecah menjadi **frontend** (nginx + UI lab interaktif) dan **backend** (Rust/axum, pool API key peserta).

```
civo-workshop/
├── backend/            # Rust API: /healthz, /api/register, /api/provision, /api/status, /api/registrations, /api/leaderboard
│   ├── Dockerfile
│   └── src/main.rs
├── frontend/           # nginx-unprivileged: static UI + proxy /api -> backend
│   ├── Dockerfile
│   ├── nginx.conf
│   └── static/index.html   # UI English: register page, 8 modules / 19 labs, guided stepper, terminal & dashboard sim
├── k8s/
│   ├── base/                     # kustomize: ns, secret, backend(+PVC), frontend(+HPA,PDB), netpol, ingress
│   ├── overlays/loadbalancer/    # alternatif: Service LoadBalancer (Civo LB), tanpa Ingress
│   └── civo-workshop-all.yaml    # hasil render base (kubectl apply -f)
└── docker-compose.yml  # local dev, topologi sama dengan k8s
```

## Build & push image

```bash
docker build -t ghcr.io/dev-cer/civo-workshop-backend:0.1.0 backend/
docker build -t ghcr.io/dev-cer/civo-workshop-frontend:0.1.0 frontend/
docker push ghcr.io/dev-cer/civo-workshop-backend:0.1.0
docker push ghcr.io/dev-cer/civo-workshop-frontend:0.1.0
```

## Deploy ke Civo

```bash
civo kubernetes create hackspace --size g4s.kube.medium --nodes 3 --cni-plugin cilium \
  --create-firewall --wait --save --merge --switch
CID=$(civo kubernetes show hackspace -o custom -f ID)
sed -i "s/CLUSTER_ID/$CID/" k8s/base/ingress.yaml
# isi pool akun asli di k8s/base/backend-secret.yaml (atau pakai SealedSecret)
kubectl apply -k k8s/base
kubectl get pods,ingress -n civo-workshop
# buka http://workshop.<CLUSTER_ID>.k8s.civo.com
```

Tanpa Ingress: `kubectl apply -k k8s/overlays/loadbalancer`.

## Catatan desain

| Keputusan | Alasan |
|---|---|
| Backend `replicas: 1`, `strategy: Recreate` | Pool akun disimpan in-memory; >1 pod = API key ganda. Untuk scale-out pindahkan state ke Redis/Postgres. |
| Akun dari `Secret` (`WORKSHOP_ACCOUNTS=HS-01:key:hackspace-01,…` — id = **access code** di kartu check-in, cluster opsional), `ADMIN_TOKEN` untuk export | Sebelumnya hard-coded di `main.rs`. Ganti `change-me-please` sebelum deploy. |
| Registrasi → akun instan, data di PVC (JSONL) | Lead capture untuk Campaign Civo; assignment bertahan saat restart. |
| Sticky assignment (`?participant=<uuid>`) | Sebelumnya setiap refresh halaman menghabiskan 1 akun pool. |
| Frontend nginx terpisah, HPA 2–6, PDB | UI stateless, bisa di-scale & jadi materi lab Autoscaling. |
| NetworkPolicy backend ← frontend only | Butuh Cilium; dipakai di lab Security. |
| Pod non-root, read-only rootfs, drop ALL caps | Baseline hardening (Pod Security "restricted"). |
| UI jatuh ke **mode demo** bila `/api` tidak terjangkau | Halaman tetap bisa dipakai sebagai self-paced lab / dihosting statis. |

## Registrasi = Google Form

Form landing page dipetakan 1:1 ke Google Form "Webinar Civo" (19 Nov 2026):
`https://docs.google.com/forms/d/e/1FAIpQLScbsMj4luKxGw4Mq-btky6hVIMZXHEYfLxfPyJoZHi_xWG8mw`

| Field | entry |
|---|---|
| First Name | 142037168 |
| Last Name | 667245164 |
| Company Name | 2057585228 |
| Job Title | 1649151384 |
| Business Email | 1866537327 |
| WhatsApp Number | 556213113 |
| Location | 1979180060 |
| Using Kubernetes? (Yes / Not yet, but interested… / Not currently… / Not sure) | 1685275686 |

Submit mengirim `POST …/formResponse` (mode `no-cors`) → respons tersimpan di Google Form/Sheet, lalu `POST /api/register` untuk akun lab + lead score. Jika Google Form tak terjangkau (mis. di pratinjau artifact yang memblokir request lintas domain), peserta mendapat tombol **Confirm on Google Form** berisi link pre-filled. Kalau pertanyaan di Google Form diubah, perbarui objek `GFORM` di `frontend/static/index.html`.

## API

| Endpoint | Fungsi |
|---|---|
| `POST /api/register` | `{participant,name,email,company,role(job title),event,phone,location,k8s_usage,consent}` → langsung assign akun lab. Email sama → akun sama. 400 validasi, 503 kursi penuh. |
| `POST /api/register` + `access_code` | Kode cocok → akun & cluster pre-provisioned itu. 404 kode tidak dikenal, 409 kode sudah dipakai. Tanpa kode → akun bebas berikutnya. Respons berisi `cluster`. |
| `GET /api/leaderboard` | `{players:[…]}` — ranking **individu** Flash Sale 12.12: satu baris per peserta (kunci = access code), hasil *official* menggantikan self-test; urut errors ↑, p95 ↑, rps ↓. Access code tidak ditampilkan. |
| `POST /api/leaderboard` | `{name,code,p95_ms,errors,rps}` (field lama `team` masih diterima). Dengan `Authorization: Bearer $ADMIN_TOKEN` → ditandai official (dipakai `flash-sale/run-official.sh`) |
| `GET /api/status` | `{total,used,free,registered,sql_candidates,early_access}` untuk fasilitator |
| `GET /api/registrations` | Export CSV peserta (incl. WhatsApp, lokasi, status K8s) + `lead_score` (0–10) & `sql_candidate` (≥6). Header `Authorization: Bearer $ADMIN_TOKEN` |

Lead score: job title senior (CTO/Head/Manager/Lead/Architect…) 3 atau praktisi (DevOps/SRE/Platform/Engineer…) 2 + K8s 'Yes' atau 'Not yet, but interested' 3 / 'Not sure' 1 + email domain perusahaan 2 + nama perusahaan terisi 1.
| `POST /api/provision` | simulasi provisioning |
| `GET /api/get-account` | legacy (tanpa registrasi) |

Registrasi ditulis ke `REGISTRATIONS_FILE` (JSONL di PVC `workshop-data`, `/data/registrations.jsonl`) dan dipulihkan saat pod restart.

```bash
kubectl -n civo-workshop port-forward svc/workshop-frontend 8080:80
curl -H "Authorization: Bearer <ADMIN_TOKEN>" localhost:8080/api/registrations > leads.csv
```

## 26 Nov · Beyond Scaling (event mode)

- Portal **Enter Launchpad**: nama, email, access code (kartu check-in). Tanpa kode = take-home labs.
- Pada 26 Nov halaman `#event` tampil otomatis: M1 Connect & Ship (10:35) · M2 Right-size (10:45) · M3 Scale (10:55) · M4 Harden (11:05) · Final Flash Sale 12.12 (11:15, 15 menit).
- Tertinggal? `kubectl apply -k k8s/skip-to/<m2|m3|m4|final>` (lihat `k8s/skip-to/README.md`).
- Challenge: `flash-sale/` (k6 `smoke.js` untuk peserta, `official.js` + `run-official.sh` untuk fasilitator). Leaderboard individu di `#ev/beyond-scaling/leaderboard`; fasilitator: `run-official.sh participants.csv` (`Nama,ACCESS_CODE,http://workshop.<cluster-id>.k8s.civo.com`).
- Navigasi: menu **Events** (halaman per judul: `#ev/k8s-at-scale` take-home track webinar 19 Nov S1–S4; `#ev/beyond-scaling` missions 26 Nov M1–Final + leaderboard; `#events` hub) dan menu **Lab Library** (8 modul, 22 lab). Lab yang dibuka dari event punya breadcrumb & Next/Previous sesuai urutan event.
- TODO sebelum event: kode region Civo Indonesia (`EVENT_REGION` di `index.html`), daftar access code + cluster asli di Secret.

## Edisi (v0.4) — satu sumber, tiga artifact
`python3 tools/build_editions.py` → `dist/{webinar,workshop,academy}/index.html` (mengganti `const EDITION`).

| Edisi | Untuk | Isi | Link |
|---|---|---|---|
| `webinar` | Peserta webinar 19 Nov | Journey: Register → Live → Take-home S1–S4 (9 lab) → Klaim kredit → Workshop 26 Nov. Tanpa access code | https://claude.ai/artifact/6CTeda7UgvuHZnFXjDaK1r |
| `workshop` | Peserta offline 26 Nov | Journey: Check-in → Talk → M1–M4 + Final (8 lab) → Winners → Academy. Access code **wajib**, leaderboard individu | https://claude.ai/artifact/1NQH82ep9bAV4WzELaQR1N |
| `academy` | Partnership / academy / customer | 8 modul, 22 lab, kode partner/cohort opsional | https://claude.ai/artifact/TKmMNxxRP4DRFFUTQE1oa4 |

Progress tiap edisi disimpan terpisah (`localStorage` key `civo-hackspace-<edisi>`). Untuk deploy di cluster, copy file edisi yang dipakai ke `frontend/static/index.html` sebelum build image (atau deploy tiga frontend dengan host berbeda).

## Jendela event (v0.4.1) — "on-time" seperti Instruqt/KodeKloud
| Edisi | Buka | Tutup | Sebelum buka | Sesudah tutup |
|---|---|---|---|---|
| webinar | 19 Nov 10:00 WIB | 19 Nov 23:59 WIB | Ruang tunggu + countdown | Halaman closed → Academy / event berikutnya |
| workshop | 26 Nov 10:00 WIB | 26 Nov 23:59 WIB | Ruang tunggu + countdown | Halaman closed; leaderboard final tetap bisa dilihat |
| academy | selalu buka | — | — | — |

- Topbar menampilkan timer `Closes in …` (kuning saat < 1 jam).
- Frontend: konstanta `WINDOW` per edisi; jika backend ada, waktu & jendela diambil dari `GET /api/window` (anti jam laptop salah).
- Backend: env `EVENT_OPEN` / `EVENT_CLOSE` (RFC3339). Di luar jendela `POST /api/register` dan self-submit leaderboard → 403. `Authorization: Bearer $ADMIN_TOKEN` tetap bisa (dry-run fasilitator, official score).
- Dry-run fasilitator di browser: buka URL dengan `?preview=1` (matikan: `?preview=0`). Hanya membuka tampilan; backend tetap mengunci.

## Certificates (QR-verified)

After the facilitator stops the race (or the event window closes), every participant who passed M0–M2
(Participation) or all six missions (Completion) gets a certificate with their registered name:

| Path | What |
|---|---|
| `POST /api/cert/me` `{email, token}` | Certificate type, ID and links for the signed-in participant |
| `GET /cert/<ID>` | A4 landscape certificate → **Download PDF** (browser print), Add to LinkedIn |
| `GET /verify/<ID>` | Public verification page — the QR code on the certificate points here |

Certificate IDs (`BTC-2026-XXXX-XXXX`) are derived from `CERT_SECRET` (Secret `launchpad-cert`, created once by
`deploy-launchpad.ps1`). Do not rotate it after the event, or every QR code stops verifying. The signature is the QR
code plus the signer's printed name — no handwritten signature image is stored anywhere.
`GET /api/race/export` now adds `certificate_id` and `certificate_url` columns for the thank-you e-mail.

### Certifier (optional)

With Secret `launchpad-certifier` (`CERTIFIER_TOKEN`, `CERTIFIER_GROUP_COMPLETION`, `CERTIFIER_GROUP_PARTICIPATION`),
official certificates are sent by Certifier (`POST /v1/credentials/create-issue-send`) instead of `/cert/<ID>`:

| Path | What |
|---|---|
| `POST /api/admin/certifier` `{action:"status"}` | Credentials sent so far |
| `POST /api/admin/certifier` `{action:"issue", dry_run?, email?}` | Send to everyone eligible and not yet sent (race must be stopped). Idempotent; upgrades Participation → Completion |

Sent credentials are recorded in `/data/certifier.json`. One automatic pass runs one minute after `EVENT_CLOSE`.
