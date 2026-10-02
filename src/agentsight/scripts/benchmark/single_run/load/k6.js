import http from 'k6/http';
import { check } from 'k6';
import { Counter, Trend } from 'k6/metrics';

const requestCount = new Counter('benchmark_requests');
const successfulRequests = new Counter('benchmark_http_success');
const timedOutRequests = new Counter('benchmark_timeouts');
const requestLatency = new Trend('benchmark_latency', true);

const qps = Number(__ENV.QPS || 100);
const duration = __ENV.DURATION || '30s';
const baseUrl = __ENV.BASE_URL || 'https://127.0.0.1:8443';
const endpoint = __ENV.ENDPOINT || '/v1/chat/completions';
const payloadBytes = Number(__ENV.PAYLOAD_KB || 4) * 1024;
const runId = __ENV.RUN_ID || 'manual';
const maxVUs = Number(
  __ENV.MAX_VUS ||
    Math.min(Math.max(100, qps * 2), Number(__ENV.BENCHMARK_MAX_VUS || 256)),
);
const preAllocatedVUs = Number(
  __ENV.PRE_ALLOCATED_VUS || Math.min(Math.max(10, Math.ceil(qps / 10)), maxVUs),
);

export const options = {
  scenarios: {
    llm: {
      executor: 'constant-arrival-rate',
      rate: qps,
      timeUnit: '1s',
      duration,
      preAllocatedVUs,
      maxVUs,
    },
  },
  insecureSkipTLSVerify: true,
  thresholds: {
    http_req_failed: ['rate<0.001'],
  },
};

function payload(requestId) {
  const padding = 'x'.repeat(Math.max(0, payloadBytes - 180));
  return JSON.stringify({
    request_id: requestId,
    model: 'test-model',
    stream: true,
    messages: [{ role: 'user', content: `hello ${padding}` }],
  });
}

export default function () {
  const requestId = `bench-${runId}-${__VU}-${__ITER}-${Date.now()}`;
  // Keep the correlation ID only in the JSON body. A per-request header can be
  // promoted to a tag by k6 extensions or local configuration, creating one
  // time series per request and making long campaigns consume unbounded RAM.
  const response = http.post(`${baseUrl}${endpoint}`, payload(requestId), {
    headers: {
      'Content-Type': 'application/json',
      'X-Benchmark-Run-ID': runId,
    },
  });
  const ok = check(response, { 'HTTP status is 2xx': (r) => r.status >= 200 && r.status < 300 });
  requestCount.add(1);
  requestLatency.add(response.timings.duration);
  if (response.error_code === 1050 || String(response.error || '').toLowerCase().includes('timeout')) {
    timedOutRequests.add(1);
  }
  if (ok) successfulRequests.add(1);
}
