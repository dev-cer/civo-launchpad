// k6 smoke test for the Flash Sale 12.12 Challenge (participants)
import http from 'k6/http';
import { check, sleep } from 'k6';

const TARGET = __ENV.TARGET;
export const options = {
  scenarios: {
    sale: { executor: 'ramping-vus', startVUs: 50, stages: [{ duration: '20s', target: 200 }, { duration: '30s', target: 500 }, { duration: '10s', target: 0 }] },
  },
  thresholds: { http_req_failed: ['rate<0.01'], http_req_duration: ['p(95)<300'] },
};
export default function () {
  const r = http.get(`${TARGET}/`);
  check(r, { 'status 200': (res) => res.status === 200 });
  sleep(0.2);
}
