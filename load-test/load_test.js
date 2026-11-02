import http from 'k6/http';
import {check, sleep} from 'k6';
import {summarize, trendStats} from './summary.js';

const BASE_URL = __ENV.BASE_URL || 'http://localhost:9001';

export let options = {
    summaryTrendStats: trendStats,
    thresholds: {
        http_req_failed: ['rate<0.01'], // http errors should be less than 1%
        http_req_duration: ['p(95)<200'], // 95% of requests should be below 200ms
    },
    stages: [
        { duration: '30s', target: 50 }, // Boost to 50 users for the time period of N
        { duration: '1m', target: 50 },  // Lock 50 users for the time period of N
        { duration: '30s', target: 0 },  // Drop users rate to 0 slowly for the time period of N
    ],
};

export default function () {
    // Страницы нумеруются с 1: в Go-версии сценария была страница 0, она всегда даёт 422.
    let res = http.get(`${BASE_URL}/users/1/100`);
    check(res, {
        'status is 200': (r) => r.status === 200,
    });
    sleep(1);
}

export function handleSummary(data) {
    return summarize(data, 'load');
}
