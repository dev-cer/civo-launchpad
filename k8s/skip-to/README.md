# Skip-to overlays (Workshop 26 Nov · Beyond Scaling)

Participants who fall behind apply one overlay to reach the start state of a mission:

| Join at | Command | State |
|---|---|---|
| M2 Right-size | `kubectl apply -k k8s/skip-to/m2` | Sample app deployed (= k8s/base) |
| M3 Scale | `kubectl apply -k k8s/skip-to/m3` | + frontend Guaranteed QoS (requests = limits) |
| M4 Harden | `kubectl apply -k k8s/skip-to/m4` + `civo kubernetes node-pool scale <cluster> <pool> --nodes 4` | + 4 nodes |
| Final | `kubectl apply -k k8s/skip-to/final` | + HPA minReplicas 3 / maxReplicas 8 |
