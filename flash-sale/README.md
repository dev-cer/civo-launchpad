# Flash Sale 12.12 Challenge
- Duration: 15 minutes · traffic ramps from 50 to 500 virtual users
- Target: http://workshop.<cluster-id>.k8s.civo.com
- Score: 1) failed requests (lower wins)  2) p95 latency (lower wins)
- Allowed: any change to your own cluster. Not allowed: touching other participants' clusters.
- Self-test: `k6 run -e TARGET=http://workshop.<cluster-id>.k8s.civo.com flash-sale/smoke.js`
- Facilitator: `./flash-sale/run-official.sh participants.csv` posts official results to /api/leaderboard
