// Общая часть сценариев: итоговый JSON в k6-results/<сценарий>-<цель>.json и краткая сводка в stdout.
// `target` — метка стенда (rust, go), передаётся как TARGET.

const PERCENTILES = ['med', 'p(95)', 'p(99)'];

// Подключается в options сценариев: по умолчанию k6 не считает p(99).
export const trendStats = ['avg', 'min', 'med', 'max', 'p(90)', 'p(95)', 'p(99)'];

function metricValues(data, name) {
    const metric = data.metrics[name];
    return metric ? metric.values : null;
}

export function summarize(data, scenario) {
    const target = __ENV.TARGET || 'rust';
    const lines = [`scenario=${scenario} target=${target}`];

    const reqs = metricValues(data, 'http_reqs');
    if (reqs) {
        lines.push(`http_reqs: count=${reqs.count} rate=${reqs.rate.toFixed(1)}/s`);
    }
    // Латентность успешных ответов (expected_response:true), если сценарий размечает ответы.
    const ok = metricValues(data, 'http_req_duration{expected_response:true}') || metricValues(data, 'http_req_duration');
    if (ok) {
        lines.push('latency ms: ' + PERCENTILES.map((p) => `${p}=${Number(ok[p]).toFixed(1)}`).join(' '));
    }
    for (const name of ['shed', 'unexpected', 'http_req_failed']) {
        const values = metricValues(data, name);
        if (values) {
            lines.push(`${name}: rate=${(values.rate === undefined ? values.value : values.rate).toFixed(4)}`);
        }
    }
    for (const [name, metric] of Object.entries(data.metrics)) {
        if (metric.thresholds) {
            for (const [expr, result] of Object.entries(metric.thresholds)) {
                lines.push(`threshold ${name} ${expr}: ${result.ok ? 'ok' : 'FAILED'}`);
            }
        }
    }

    return {
        stdout: lines.join('\n') + '\n',
        [`k6-results/${scenario}-${target}.json`]: JSON.stringify(data, null, 2),
    };
}
