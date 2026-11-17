// k6 official run (facilitator): 15 minutes against one participant cluster
import http from 'k6/http';
import { check } from 'k6';
const TARGET = __ENV.TARGET;
export const options = {
  scenarios: { sale: { executor: 'ramping-vus', startVUs: 50, stages: [{ duration: '3m', target: 300 }, { duration: '9m', target: 500 }, { duration: '3m', target: 50 }] } },
  summaryTrendStats: ['avg', 'p(95)'],
};
export default function () { check(http.get(`${TARGET}/`), { ok: (r) => r.status === 200 }); }
