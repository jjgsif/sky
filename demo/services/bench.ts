import { Service, Handler } from "sky-framework/decorators";
import * as path from "node:path";

const BENCH_FILE = path.join(process.cwd(), "bench", "CLAUDE.md");
const CACHE_TTL_MS = 30_000;

interface LatencyPercentiles {
    p50: string;
    p75: string;
    p90: string;
    p95: string;
    p99: string;
    p999: string;
}

interface ScenarioResult {
    scenario: string;
    requestsPerSec: string;
    totalRequests: string;
    latency: LatencyPercentiles;
}

interface StandardBenchmark {
    timestamp: string;
    commit: string;
    settings: Record<string, string>;
    scenarios: ScenarioResult[];
}

interface RampRow {
    targetRps: number;
    actualRps: number;
    p50: string;
    p99: string;
    status: string;
}

interface RampResult {
    timestamp: string;
    saturationPoint: string | null;
    peakSustainable: string | null;
    rows: RampRow[];
}

interface TtfbEndpoint {
    endpoint: string;
    rows: Array<{ percentile: string; ttfbMs: string }>;
}

interface TtfbResult {
    timestamp: string;
    runs: string;
    concurrency: string;
    endpoints: TtfbEndpoint[];
}

interface BenchResults {
    latestStandard: StandardBenchmark | null;
    latestRamp: RampResult | null;
    latestTtfb: TtfbResult | null;
    error?: string;
}

let cachedResult: BenchResults | null = null;
let cacheTs = 0;

@Service({ lifetime: "singleton" })
export class BenchmarkService {
    @Handler({ method: "GET", path: "/bench/results", validate: false })
    async results() {
        const now = Date.now();
        if (cachedResult && now - cacheTs < CACHE_TTL_MS) return cachedResult;
        const result = await loadBenchResults();
        cachedResult = result;
        cacheTs = now;
        return result;
    }
}

async function loadBenchResults(): Promise<BenchResults> {
    let text: string;
    try {
        text = await Bun.file(BENCH_FILE).text();
    } catch {
        return {
            latestStandard: null,
            latestRamp: null,
            latestTtfb: null,
            error: "bench/CLAUDE.md not found",
        };
    }
    return {
        latestStandard: parseLatestStandard(text),
        latestRamp: parseLatestRamp(text),
        latestTtfb: parseLatestTtfb(text),
    };
}

function parseLatencyPercentiles(block: string): LatencyPercentiles {
    const pick = (label: string) => {
        const m = block.match(new RegExp(`\\b${label}\\s*:\\s*([\\d.]+\\s*ms)`));
        return m?.[1]?.trim() ?? "";
    };
    return {
        p50: pick("p50"),
        p75: pick("p75"),
        p90: pick("p90"),
        p95: pick("p95"),
        p99: pick("p99"),
        p999: pick("p99\\.9"),
    };
}

function parseLatestStandard(text: string): StandardBenchmark | null {
    const headerRe = /^## Standard benchmark — (.+? UTC) — `([^`]+)`.*$/gm;
    let last: RegExpExecArray | null = null;
    let m: RegExpExecArray | null;
    while ((m = headerRe.exec(text)) !== null) last = m;
    if (!last) return null;

    const timestamp = last[1] ?? "";
    const commit = last[2] ?? "";
    const sectionStart = last.index;
    const nextH2 = text.indexOf("\n## ", sectionStart + 1);
    const sectionText = nextH2 === -1 ? text.slice(sectionStart) : text.slice(sectionStart, nextH2);

    // Settings table — rows outside the code block
    const settings: Record<string, string> = {};
    for (const row of sectionText.matchAll(/^\| ([^|]+?)\s*\| ([^|]+?)\s*\|$/gm)) {
        const key = (row[1] ?? "").trim();
        const val = (row[2] ?? "").trim();
        if (key !== "Setting" && !key.startsWith("-")) {
            settings[key] = val;
        }
    }

    // Scenario blocks: bounded by ### headings
    const scenarioMatches = [...sectionText.matchAll(/^### (.+?)$/gm)];
    const scenarios: ScenarioResult[] = [];

    for (let i = 0; i < scenarioMatches.length; i++) {
        const sm = scenarioMatches[i]!;
        const start = sm.index!;
        const end = i + 1 < scenarioMatches.length
            ? scenarioMatches[i + 1]!.index!
            : sectionText.length;
        const chunk = sectionText.slice(start, end);

        const rpsMatch = chunk.match(/^Requests\/sec:\s+([\d.]+)/m);
        const reqMatch = chunk.match(/(\d+) requests in/);

        // Latency block header differs between unary and streaming
        const latencyBlockMatch = chunk.match(
            /──[^─]*(?:Latency percentiles|Full-stream completion latency)[^─]*──+\n([\s\S]+?)─{20,}/
        );
        if (!latencyBlockMatch) continue;

        scenarios.push({
            scenario: sm[1] ?? "",
            requestsPerSec: rpsMatch?.[1] ?? "",
            totalRequests: reqMatch?.[1] ?? "",
            latency: parseLatencyPercentiles(latencyBlockMatch[1] ?? ""),
        });
    }

    return { timestamp, commit, settings, scenarios };
}

function parseRampTable(section: string): RampRow[] {
    const rows: RampRow[] = [];
    // Match data rows: | 280000 | 279621 | 0.96ms | 5.35ms | OK |
    for (const row of section.matchAll(
        /^\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*([^|]+?)\s*\|\s*([^|]+?)\s*\|\s*(\w+)\s*\|/gm
    )) {
        rows.push({
            targetRps: parseInt(row[1] ?? "0", 10),
            actualRps: parseInt(row[2] ?? "0", 10),
            p50: (row[3] ?? "").trim(),
            p99: (row[4] ?? "").trim(),
            status: (row[5] ?? "").trim(),
        });
    }
    return rows;
}

function parseLatestRamp(text: string): RampResult | null {
    const headerRe = /^### Ramp Results — (.+? UTC)$/gm;
    const allMatches: RegExpExecArray[] = [];
    let m: RegExpExecArray | null;
    while ((m = headerRe.exec(text)) !== null) allMatches.push(m);
    if (!allMatches.length) return null;

    // Scan newest-to-oldest; pick first run where every row has actualRps > 0
    for (let i = allMatches.length - 1; i >= 0; i--) {
        const hm = allMatches[i]!;
        const sectionStart = hm.index;
        const nextRamp = i + 1 < allMatches.length ? allMatches[i + 1]!.index : text.length;
        const nextH2 = text.indexOf("\n## ", sectionStart + 1);
        const end = nextH2 !== -1 && nextH2 < nextRamp ? nextH2 : nextRamp;
        const section = text.slice(sectionStart, end);

        const rows = parseRampTable(section);
        if (rows.length > 0 && rows.every(r => r.actualRps > 0)) {
            const satMatch = section.match(/\*\*Saturation point:\s*(.+?)\*\*/);
            const peakMatch = section.match(/\*\*Peak sustainable rate:\s*(.+?)\*\*/);
            return {
                timestamp: hm[1] ?? "",
                saturationPoint: satMatch?.[1] ?? null,
                peakSustainable: peakMatch?.[1] ?? null,
                rows,
            };
        }
    }
    return null;
}

function parseLatestTtfb(text: string): TtfbResult | null {
    const headerRe = /^### TTFB Results — (.+? UTC)$/gm;
    const allMatches: RegExpExecArray[] = [];
    let m: RegExpExecArray | null;
    while ((m = headerRe.exec(text)) !== null) allMatches.push(m);
    if (!allMatches.length) return null;

    const hm = allMatches[allMatches.length - 1]!;
    const sectionStart = hm.index;
    // End at next ## or ### heading
    const hmFullLen = hm[0]!.length;
    const rest = text.slice(sectionStart + hmFullLen);
    const nextSection = rest.search(/^#{2,3} /m);
    const section = nextSection === -1
        ? text.slice(sectionStart)
        : text.slice(sectionStart, sectionStart + hmFullLen + nextSection);

    const runsMatch = section.match(/Runs:\s*(\d+)\s*\|\s*Concurrency:\s*(\d+)/);

    // Collect #### TTFB subsection positions
    const epHeaders: RegExpExecArray[] = [];
    const epRe = /^#### TTFB — (.+)$/gm;
    let ep: RegExpExecArray | null;
    while ((ep = epRe.exec(section)) !== null) epHeaders.push(ep);

    const endpoints: TtfbEndpoint[] = [];
    for (let j = 0; j < epHeaders.length; j++) {
        const eh = epHeaders[j]!;
        const epEnd = j + 1 < epHeaders.length ? epHeaders[j + 1]!.index! : section.length;
        const epText = section.slice(eh.index!, epEnd);

        const rows: Array<{ percentile: string; ttfbMs: string }> = [];
        for (const row of epText.matchAll(/^\|\s*(\w+)\s*\|\s*([\d.]+)\s*\|/gm)) {
            const percentile = (row[1] ?? "").trim();
            if (percentile !== "Percentile") {
                rows.push({ percentile, ttfbMs: (row[2] ?? "").trim() });
            }
        }
        if (rows.length > 0) {
            endpoints.push({ endpoint: (eh[1] ?? "").trim(), rows });
        }
    }

    return {
        timestamp: hm[1] ?? "",
        runs: runsMatch?.[1] ?? "",
        concurrency: runsMatch?.[2] ?? "",
        endpoints,
    };
}
