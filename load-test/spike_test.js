import http from 'k6/http';
import {check} from 'k6';
import {Rate} from 'k6/metrics';
import {summarize, trendStats} from './summary.js';

const BASE_URL = __ENV.BASE_URL || 'http://localhost:9001';

// Только 200 считается ожидаемым ответом: 503 (отказ при перегрузке) и прочие коды
// попадают в expected_response:false и не искажают латентность успешных ответов.
http.setResponseCallback(http.expectedStatuses(200));

const shed = new Rate('shed');             // доля 503
const unexpected = new Rate('unexpected'); // доля прочих не-200

export const options = {
    summaryTrendStats: trendStats,
    scenarios: {
        spike: {
            executor: 'ramping-arrival-rate',
            startRate: 50,
            timeUnit: '1s',
            preAllocatedVUs: 500,
            maxVUs: 2000,
            stages: [
                { duration: '30s', target: 50 },   // базовая нагрузка
                { duration: '5s', target: 2000 },  // скачок
                { duration: '30s', target: 2000 }, // пик
                { duration: '5s', target: 50 },    // спад
                { duration: '30s', target: 50 },   // восстановление
            ],
        },
    },
    // Пороги информационные: их нарушение фиксируется в сводке, но не блокирует приёмку.
    thresholds: {
        unexpected: ['rate<0.01'],
        'http_req_duration{expected_response:true}': ['p(95)<200'],
    },
};

export default function () {
    // 80% списки пользователей, 20% списки с заказами.
    const path = Math.random() < 0.8 ? '/users/1/100' : '/users-with-orders/1/100';
    const res = http.get(`${BASE_URL}${path}`);
    shed.add(res.status === 503);
    unexpected.add(res.status !== 200 && res.status !== 503);
    check(res, {
        'status is 200 or 503': (r) => r.status === 200 || r.status === 503,
    });
}

export function handleSummary(data) {
    return summarize(data, 'spike');
}
