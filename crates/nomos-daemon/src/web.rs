use crate::actor::ArbiterHandle;
use crate::server::{ArbiterRequest, ArbiterResponse};
use axum::{
    extract::{Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse,
    },
    routing::get,
    Json, Router,
};
use futures::stream::Stream;
use nomos_sys::probe::probe_host;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::StreamExt as _;
use tracing::info;

#[derive(Deserialize)]
pub struct AccountingQuery {
    pub limit: Option<usize>,
    pub days: Option<u32>,
}

const EMBEDDED_DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>NOMOS - Single-Host Resource Authority & Audit Dashboard</title>
    <style>
        :root {
            --bg-primary: #0a0d14;
            --bg-secondary: #111726;
            --bg-card: #161f33;
            --bg-hover: #1c2740;
            --border: #243049;
            --border-light: #324364;
            --text-primary: #f8fafc;
            --text-secondary: #94a3b8;
            --text-muted: #64748b;
            --accent-blue: #38bdf8;
            --accent-green: #10b981;
            --accent-amber: #f59e0b;
            --accent-purple: #a855f7;
            --accent-rose: #f43f5e;
            --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
        }
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
            background: var(--bg-primary);
            color: var(--text-primary);
            padding: 24px 32px;
            min-height: 100vh;
        }
        .container { max-width: 1280px; margin: 0 auto; }
        header {
            border-bottom: 1px solid var(--border);
            padding-bottom: 20px;
            margin-bottom: 24px;
            display: flex;
            justify-content: space-between;
            align-items: center;
            flex-wrap: wrap;
            gap: 16px;
        }
        .header-left { display: flex; align-items: center; gap: 14px; }
        .logo-box {
            background: linear-gradient(135deg, #0284c7, #6366f1);
            color: #fff;
            font-weight: 900;
            font-family: var(--font-mono);
            font-size: 16px;
            padding: 6px 12px;
            border-radius: 6px;
            letter-spacing: 1px;
            box-shadow: 0 4px 12px rgba(2, 132, 199, 0.3);
        }
        h1 { font-size: 22px; font-weight: 700; letter-spacing: -0.3px; font-family: var(--font-mono); }
        .sub-header { font-size: 13px; color: var(--text-secondary); margin-top: 2px; }
        .header-meta { display: flex; align-items: center; gap: 12px; }
        .badge {
            background: var(--bg-secondary);
            border: 1px solid var(--border);
            color: var(--accent-blue);
            font-size: 11px;
            padding: 5px 10px;
            border-radius: 6px;
            font-family: var(--font-mono);
            display: inline-flex;
            align-items: center;
            gap: 6px;
        }
        .status-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent-green); box-shadow: 0 0 8px var(--accent-green); animation: pulse 2s infinite; }
        @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }

        /* Tabs */
        .tabs {
            display: flex;
            gap: 8px;
            border-bottom: 1px solid var(--border);
            margin-bottom: 24px;
        }
        .tab-btn {
            background: transparent;
            border: none;
            color: var(--text-secondary);
            font-size: 13.5px;
            font-weight: 600;
            padding: 10px 18px;
            cursor: pointer;
            border-bottom: 2px solid transparent;
            font-family: var(--font-mono);
            transition: all 0.2s ease;
        }
        .tab-btn:hover { color: var(--text-primary); background: rgba(255,255,255,0.02); }
        .tab-btn.active {
            color: var(--accent-blue);
            border-bottom-color: var(--accent-blue);
            background: rgba(56, 189, 248, 0.05);
        }

        .tab-content { display: none; }
        .tab-content.active { display: block; }

        /* KPI Cards Grid */
        .grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
            gap: 16px;
            margin-bottom: 24px;
        }
        .card {
            background: var(--bg-secondary);
            border: 1px solid var(--border);
            border-radius: 8px;
            padding: 18px;
            font-family: var(--font-mono);
            transition: border-color 0.2s;
        }
        .card:hover { border-color: var(--border-light); }
        .card-title {
            font-size: 11px;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            color: var(--text-secondary);
            margin-bottom: 8px;
            display: flex;
            justify-content: space-between;
        }
        .card-val { font-size: 24px; font-weight: 700; color: #fff; margin-bottom: 4px; }
        .card-sub { font-size: 12px; color: var(--text-muted); }
        .progress-bg { background: #1e293b; border-radius: 4px; height: 7px; overflow: hidden; margin-top: 10px; }
        .progress-bar { height: 100%; background: var(--accent-blue); width: 0%; transition: width 0.3s ease; }

        /* Chart Canvas */
        .chart-box {
            background: var(--bg-secondary);
            border: 1px solid var(--border);
            border-radius: 8px;
            padding: 18px;
            margin-bottom: 24px;
        }
        .chart-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 12px; }
        .chart-title { font-family: var(--font-mono); font-size: 13px; font-weight: 600; color: #e2e8f0; }
        .chart-legend { display: flex; gap: 16px; font-family: var(--font-mono); font-size: 11px; color: var(--text-secondary); }
        .legend-item { display: flex; align-items: center; gap: 6px; }
        .legend-color { width: 10px; height: 10px; border-radius: 2px; }
        canvas { width: 100%; height: 120px; display: block; }

        /* Section Title & Controls */
        .section-header {
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-top: 24px;
            margin-bottom: 12px;
            flex-wrap: wrap;
            gap: 12px;
        }
        .section-title {
            font-family: var(--font-mono);
            font-size: 14px;
            font-weight: 600;
            color: #cbd5e1;
            letter-spacing: 0.5px;
        }
        .filter-bar {
            display: flex;
            gap: 10px;
            align-items: center;
        }
        .filter-input, .filter-select {
            background: var(--bg-secondary);
            border: 1px solid var(--border);
            color: var(--text-primary);
            font-family: var(--font-mono);
            font-size: 12px;
            padding: 6px 12px;
            border-radius: 5px;
            outline: none;
        }
        .filter-input:focus, .filter-select:focus { border-color: var(--accent-blue); }
        .btn-refresh {
            background: var(--bg-card);
            border: 1px solid var(--border);
            color: var(--accent-blue);
            font-family: var(--font-mono);
            font-size: 12px;
            padding: 6px 12px;
            border-radius: 5px;
            cursor: pointer;
            transition: all 0.2s;
        }
        .btn-refresh:hover { background: var(--bg-hover); border-color: var(--border-light); }

        /* Tables */
        .table-wrap {
            overflow-x: auto;
            border: 1px solid var(--border);
            border-radius: 8px;
            background: var(--bg-secondary);
        }
        table {
            width: 100%;
            border-collapse: collapse;
            font-family: var(--font-mono);
            font-size: 12px;
            text-align: left;
        }
        th, td { padding: 11px 14px; border-bottom: 1px solid var(--border); }
        th { background: #131b2e; color: #94a3b8; font-size: 11px; text-transform: uppercase; font-weight: 600; }
        tr:hover td { background: var(--bg-card); }
        tr:last-child td { border-bottom: none; }

        /* Status Tags */
        .tag { padding: 2px 7px; border-radius: 4px; font-size: 11px; font-weight: 600; display: inline-block; }
        .tag-running { background: rgba(16, 185, 129, 0.15); color: #34d399; border: 1px solid rgba(16, 185, 129, 0.3); }
        .tag-queued { background: rgba(245, 158, 11, 0.15); color: #fbbf24; border: 1px solid rgba(245, 158, 11, 0.3); }
        .tag-completed { background: rgba(56, 189, 248, 0.15); color: #38bdf8; border: 1px solid rgba(56, 189, 248, 0.3); }
        .tag-failed { background: rgba(244, 63, 94, 0.15); color: #fb7185; border: 1px solid rgba(244, 63, 94, 0.3); }

        /* Hardware specs grid */
        .hw-grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
            gap: 16px;
        }
        .hw-card {
            background: var(--bg-secondary);
            border: 1px solid var(--border);
            border-radius: 8px;
            padding: 20px;
            font-family: var(--font-mono);
        }
        .hw-card h3 { font-size: 14px; color: var(--accent-blue); margin-bottom: 12px; border-bottom: 1px solid var(--border); padding-bottom: 8px; }
        .hw-item { display: flex; justify-content: space-between; font-size: 12.5px; padding: 6px 0; border-bottom: 1px dashed rgba(255,255,255,0.06); }
        .hw-item:last-child { border-bottom: none; }
        .hw-label { color: var(--text-secondary); }
        .hw-val { color: #fff; font-weight: 600; }
    </style>
</head>
<body>
    <div class="container">
        <header>
            <div class="header-left">
                <div class="logo-box">NOMOS</div>
                <div>
                    <h1>NOMOS RESOURCE ARBITER</h1>
                    <div class="sub-header">Host Resource Authority, Workload Telemetry & Audit Dashboard</div>
                </div>
            </div>
            <div class="header-meta">
                <div class="badge" id="host-badge">HOST: CONNECTING...</div>
                <div class="badge" id="conn-status">
                    <span class="status-dot"></span>
                    <span>LIVE SSE CONNECTED</span>
                </div>
            </div>
        </header>

        <!-- Tab Navigation -->
        <div class="tabs">
            <button class="tab-btn active" onclick="switchTab('tab-live')">Live Monitor</button>
            <button class="tab-btn" onclick="switchTab('tab-history')">Job History & Audit</button>
            <button class="tab-btn" onclick="switchTab('tab-hardware')">Host Hardware & Pool</button>
        </div>

        <!-- 1. LIVE MONITOR TAB -->
        <div id="tab-live" class="tab-content active">
            <div class="grid">
                <div class="card">
                    <div class="card-title">Nomos CPU Budget <span id="cpu-cores-sub">0/0</span></div>
                    <div class="card-val" id="cpu-val">0.0 / 0.0 Cores</div>
                    <div class="progress-bg"><div class="progress-bar" id="cpu-bar"></div></div>
                    <div class="card-sub" id="cpu-pct">0% Used of Nomos Pool</div>
                </div>
                <div class="card">
                    <div class="card-title">Nomos RAM Budget <span id="mem-cores-sub">0/0</span></div>
                    <div class="card-val" id="mem-val">0.0 / 0.0 GiB</div>
                    <div class="progress-bg"><div class="progress-bar" id="mem-bar" style="background: var(--accent-green);"></div></div>
                    <div class="card-sub" id="mem-pct">0% Used of Nomos Pool</div>
                </div>
                <div class="card">
                    <div class="card-title">Active / Queued Leases</div>
                    <div class="card-val" id="leases-val">0 Active</div>
                    <div class="card-sub" id="queued-sub" style="margin-top: 10px;">0 Waiting in queue</div>
                </div>
                <div class="card">
                    <div class="card-title">30-Day Ledger Total</div>
                    <div class="card-val" id="card-total-jobs">-</div>
                    <div class="card-sub" id="card-total-cpu">- CPU Hours</div>
                </div>
            </div>

            <!-- Rolling Real-time Chart -->
            <div class="chart-box">
                <div class="chart-header">
                    <div class="chart-title">REAL-TIME RESOURCE UTILIZATION (Last 60s)</div>
                    <div class="chart-legend">
                        <div class="legend-item"><div class="legend-color" style="background: var(--accent-blue);"></div> CPU %</div>
                        <div class="legend-item"><div class="legend-color" style="background: var(--accent-green);"></div> RAM %</div>
                    </div>
                </div>
                <canvas id="liveChart" height="110"></canvas>
            </div>

            <div class="section-header">
                <div class="section-title">ACTIVE WORKER LEASES (REAL-TIME POOL)</div>
                <div style="font-size: 11px; color: var(--text-muted); font-family: var(--font-mono);">Updates live via SSE stream</div>
            </div>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>Lease ID</th>
                            <th>Worker ID</th>
                            <th>Tenant</th>
                            <th>CPU (Cores)</th>
                            <th>RAM</th>
                            <th>Priority</th>
                            <th>State</th>
                        </tr>
                    </thead>
                    <tbody id="leases-tbody">
                        <tr><td colspan="7" style="text-align: center; color: var(--text-muted); padding: 20px;">No active worker leases</td></tr>
                    </tbody>
                </table>
            </div>
        </div>

        <!-- 2. JOB HISTORY & AUDIT TAB -->
        <div id="tab-history" class="tab-content">
            <div class="grid">
                <div class="card">
                    <div class="card-title">Total Tracked Jobs</div>
                    <div class="card-val" id="hist-total-jobs" style="color: var(--accent-blue);">-</div>
                    <div class="card-sub">Recorded in ledger</div>
                </div>
                <div class="card">
                    <div class="card-title">Cumulative CPU-Hours</div>
                    <div class="card-val" id="hist-cpu-hours" style="color: var(--accent-green);">-</div>
                    <div class="card-sub">Total computed core-time</div>
                </div>
                <div class="card">
                    <div class="card-title">Peak Memory Recorded</div>
                    <div class="card-val" id="hist-peak-ram" style="color: var(--accent-amber);">-</div>
                    <div class="card-sub">Highest watermark seen</div>
                </div>
                <div class="card">
                    <div class="card-title">Cumulative Runtime</div>
                    <div class="card-val" id="hist-duration-hours" style="color: var(--accent-purple);">-</div>
                    <div class="card-sub">Wall-clock task execution</div>
                </div>
            </div>

            <div class="section-header">
                <div class="section-title">RECENT COMPLETED WORKERS & JOBS</div>
                <div class="filter-bar">
                    <input type="text" id="hist-search" class="filter-input" placeholder="Filter worker / lease..." oninput="renderHistoryTable()">
                    <select id="hist-state-filter" class="filter-select" onchange="renderHistoryTable()">
                        <option value="ALL">All States</option>
                        <option value="Completed">Completed</option>
                        <option value="Terminated">Terminated / Killed</option>
                        <option value="Timeout">Timeout</option>
                    </select>
                    <button class="btn-refresh" onclick="fetchAccounting()">↻ Refresh</button>
                </div>
            </div>

            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th>Lease ID</th>
                            <th>Worker ID</th>
                            <th>Tenant</th>
                            <th>Duration</th>
                            <th>CPU-Sec</th>
                            <th>Peak RAM</th>
                            <th>Finished At</th>
                            <th>State</th>
                        </tr>
                    </thead>
                    <tbody id="history-tbody">
                        <tr><td colspan="8" style="text-align: center; color: var(--text-muted); padding: 20px;">Loading accounting ledger...</td></tr>
                    </tbody>
                </table>
            </div>
        </div>

        <!-- 3. HOST HARDWARE & POOL TAB -->
        <div id="tab-hardware" class="tab-content">
            <div class="hw-grid">
                <div class="hw-card">
                    <h3>PHYSICAL HOST TELEMETRY</h3>
                    <div class="hw-item"><span class="hw-label">Physical CPU Cores</span><span class="hw-val" id="hw-cpu-cores">-</span></div>
                    <div class="hw-item"><span class="hw-label">Physical RAM</span><span class="hw-val" id="hw-ram-total">-</span></div>
                    <div class="hw-item"><span class="hw-label">Physical Swap Space</span><span class="hw-val" id="hw-swap-total">-</span></div>
                    <div class="hw-item"><span class="hw-label">Total Disk Storage</span><span class="hw-val" id="hw-storage-total">-</span></div>
                </div>
                <div class="hw-card">
                    <h3>HARDWARE ACCELERATORS</h3>
                    <div class="hw-item"><span class="hw-label">GPU Accelerator</span><span class="hw-val" id="hw-gpu-status">-</span></div>
                    <div class="hw-item"><span class="hw-label">GPU Device Model</span><span class="hw-val" id="hw-gpu-model">-</span></div>
                    <div class="hw-item"><span class="hw-label">NPU Accelerator</span><span class="hw-val" id="hw-npu-status">-</span></div>
                    <div class="hw-item"><span class="hw-label">NPU Device Model</span><span class="hw-val" id="hw-npu-model">-</span></div>
                </div>
                <div class="hw-card">
                    <h3>NOMOS POOL ALLOCATION LIMITS</h3>
                    <div class="hw-item"><span class="hw-label">Max Nomos CPU Cores</span><span class="hw-val" id="pool-cores-max">-</span></div>
                    <div class="hw-item"><span class="hw-label">Max Nomos RAM Pool</span><span class="hw-val" id="pool-mem-max">-</span></div>
                    <div class="hw-item"><span class="hw-label">Host Safe Memory Reserve</span><span class="hw-val">8.00 GiB Floor</span></div>
                    <div class="hw-item"><span class="hw-label">IPC Domain Socket</span><span class="hw-val">/run/nomos/arbiter.sock</span></div>
                </div>
            </div>
        </div>
    </div>

    <script>
        // State
        let allHistoryRecords = [];
        let historySummary = null;
        let chartData = { cpu: Array(60).fill(0), mem: Array(60).fill(0) };

        // Tab Switching
        function switchTab(tabId) {
            document.querySelectorAll(".tab-btn").forEach(btn => btn.classList.remove("active"));
            document.querySelectorAll(".tab-content").forEach(c => c.classList.remove("active"));
            event.target.classList.add("active");
            document.getElementById(tabId).classList.add("active");

            if (tabId === "tab-history" && allHistoryRecords.length === 0) {
                fetchAccounting();
            }
        }

        function formatBytes(bytes) {
            if (!bytes || bytes === 0) return "0 B";
            const k = 1024;
            const sizes = ["B", "KiB", "MiB", "GiB", "TiB"];
            const i = Math.floor(Math.log(bytes) / Math.log(k));
            return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
        }

        function formatTime(secs) {
            if (secs === undefined || secs === null) return "0.0s";
            if (secs < 60) return secs.toFixed(2) + "s";
            const m = Math.floor(secs / 60);
            const s = (secs % 60).toFixed(1);
            return `${m}m ${s}s`;
        }

        // Live SSE Setup
        const evtSource = new EventSource("/api/events");
        evtSource.onmessage = function(event) {
            const data = JSON.parse(event.data);
            const pool = data.pool;

            // CPU Gauge
            const cpuUsed = data.allocated_cpu;
            const cpuTotal = pool.total_cores;
            const cpuPct = cpuTotal > 0 ? (cpuUsed / cpuTotal * 100).toFixed(1) : 0;
            document.getElementById("cpu-val").innerText = `${cpuUsed.toFixed(1)} / ${cpuTotal.toFixed(1)} Cores`;
            document.getElementById("cpu-bar").style.width = `${Math.min(cpuPct, 100)}%`;
            document.getElementById("cpu-pct").innerText = `${cpuPct}% of Nomos Pool`;
            document.getElementById("cpu-cores-sub").innerText = `${cpuPct}%`;

            // RAM Gauge
            const memUsedBytes = data.allocated_memory_bytes;
            const memTotalBytes = pool.total_memory_bytes;
            const memPct = memTotalBytes > 0 ? (memUsedBytes / memTotalBytes * 100).toFixed(1) : 0;
            document.getElementById("mem-val").innerText = `${formatBytes(memUsedBytes)} / ${formatBytes(memTotalBytes)}`;
            document.getElementById("mem-bar").style.width = `${Math.min(memPct, 100)}%`;
            document.getElementById("mem-pct").innerText = `${memPct}% of Nomos Pool`;
            document.getElementById("mem-cores-sub").innerText = `${memPct}%`;

            // Leases
            document.getElementById("leases-val").innerText = `${data.active_leases.length} Active`;
            document.getElementById("queued-sub").innerText = `${data.queued_leases.length} Waiting in queue`;

            // Pool hardware tab values
            document.getElementById("pool-cores-max").innerText = `${cpuTotal.toFixed(1)} Cores`;
            document.getElementById("pool-mem-max").innerText = formatBytes(memTotalBytes);

            // Active Leases Table
            const tbody = document.getElementById("leases-tbody");
            if (data.active_leases.length === 0) {
                tbody.innerHTML = '<tr><td colspan="7" style="text-align: center; color: var(--text-muted); padding: 20px;">No active worker leases</td></tr>';
            } else {
                tbody.innerHTML = data.active_leases.map(l => `
                    <tr>
                        <td style="color: var(--accent-blue); font-weight: 600;">${l.id}</td>
                        <td style="color: #fff; font-weight: 500;">${l.request.worker_id}</td>
                        <td style="color: var(--text-secondary);">${l.request.tenant}</td>
                        <td>${l.request.req_cpu.toFixed(1)}</td>
                        <td>${formatBytes(l.request.req_memory_bytes)}</td>
                        <td>${l.request.priority}</td>
                        <td><span class="tag tag-running">${l.state}</span></td>
                    </tr>
                `).join("");
            }

            // Update real-time chart buffer
            chartData.cpu.push(parseFloat(cpuPct));
            chartData.cpu.shift();
            chartData.mem.push(parseFloat(memPct));
            chartData.mem.shift();
            drawChart();
        };

        evtSource.onerror = function() {
            const el = document.getElementById("conn-status");
            el.innerHTML = '<span class="status-dot" style="background: var(--accent-amber); box-shadow: 0 0 8px var(--accent-amber);"></span> RECONNECTING...';
        };

        // Draw Smooth Canvas Chart
        function drawChart() {
            const canvas = document.getElementById("liveChart");
            if (!canvas) return;
            const ctx = canvas.getContext("2d");
            const w = canvas.width = canvas.parentElement.clientWidth - 36;
            const h = canvas.height = 110;

            ctx.clearRect(0, 0, w, h);

            // Grid lines
            ctx.strokeStyle = "rgba(255, 255, 255, 0.05)";
            ctx.lineWidth = 1;
            for (let y = 0; y <= 100; y += 25) {
                const py = h - (y / 100 * (h - 20)) - 10;
                ctx.beginPath();
                ctx.moveTo(0, py);
                ctx.lineTo(w, py);
                ctx.stroke();
            }

            function plotSeries(arr, color) {
                ctx.strokeStyle = color;
                ctx.lineWidth = 2;
                ctx.beginPath();
                const step = w / (arr.length - 1);
                arr.forEach((val, i) => {
                    const x = i * step;
                    const y = h - (val / 100 * (h - 20)) - 10;
                    if (i === 0) ctx.moveTo(x, y);
                    else ctx.lineTo(x, y);
                });
                ctx.stroke();
            }

            plotSeries(chartData.cpu, "#38bdf8");
            plotSeries(chartData.mem, "#10b981");
        }

        // Fetch Accounting History
        async function fetchAccounting() {
            try {
                const res = await fetch("/api/accounting?limit=100&days=30");
                const data = await res.json();
                if (data.status === "ok") {
                    historySummary = data.summary;
                    allHistoryRecords = data.recent || [];

                    // Update summary cards
                    document.getElementById("card-total-jobs").innerText = `${data.summary.total_records.toLocaleString()}`;
                    document.getElementById("card-total-cpu").innerText = `${data.summary.total_cpu_hours.toFixed(3)} CPU-Hrs`;

                    document.getElementById("hist-total-jobs").innerText = `${data.summary.total_records.toLocaleString()}`;
                    document.getElementById("hist-cpu-hours").innerText = `${data.summary.total_cpu_hours.toFixed(3)} hrs`;
                    document.getElementById("hist-peak-ram").innerText = formatBytes(data.summary.peak_memory_seen_bytes);
                    document.getElementById("hist-duration-hours").innerText = `${data.summary.total_duration_hours.toFixed(2)} hrs`;

                    renderHistoryTable();
                }
            } catch (err) {
                console.error("Failed to fetch accounting:", err);
            }
        }

        // Render Filtered History Table
        function renderHistoryTable() {
            const query = (document.getElementById("hist-search").value || "").toLowerCase().trim();
            const stateFilter = document.getElementById("hist-state-filter").value;

            const tbody = document.getElementById("history-tbody");
            const filtered = allHistoryRecords.filter(r => {
                const matchesText = !query || r.worker_id.toLowerCase().includes(query) || r.lease_id.toLowerCase().includes(query);
                const matchesState = stateFilter === "ALL" || r.final_state.toLowerCase().includes(stateFilter.toLowerCase());
                return matchesText && matchesState;
            });

            if (filtered.length === 0) {
                tbody.innerHTML = '<tr><td colspan="8" style="text-align: center; color: var(--text-muted); padding: 20px;">No matching audit records found</td></tr>';
                return;
            }

            tbody.innerHTML = filtered.map(r => {
                let tagClass = "tag-completed";
                if (r.final_state.toLowerCase().includes("fail") || r.final_state.toLowerCase().includes("kill")) tagClass = "tag-failed";
                else if (r.final_state.toLowerCase().includes("time")) tagClass = "tag-queued";

                const dateStr = new Date(r.finished_at).toLocaleString();

                return `
                    <tr>
                        <td style="color: var(--accent-blue); font-weight: 600;">${r.lease_id}</td>
                        <td style="color: #fff; font-weight: 500;">${r.worker_id}</td>
                        <td style="color: var(--text-secondary);">${r.tenant}</td>
                        <td>${formatTime(r.duration_seconds)}</td>
                        <td>${r.cpu_core_seconds.toFixed(2)}s</td>
                        <td>${formatBytes(r.peak_memory_bytes)}</td>
                        <td style="color: var(--text-muted); font-size: 11px;">${dateStr}</td>
                        <td><span class="tag ${tagClass}">${r.final_state}</span></td>
                    </tr>
                `;
            }).join("");
        }

        // Fetch Host Hardware Telemetry
        async function fetchHardware() {
            try {
                const res = await fetch("/api/probe");
                const data = await res.json();
                const r = data.resources;

                document.getElementById("host-badge").innerText = `HOST: ${r.total_cores} CORES | ${formatBytes(r.total_memory_bytes)} RAM`;
                document.getElementById("hw-cpu-cores").innerText = `${r.total_cores} Cores`;
                document.getElementById("hw-ram-total").innerText = formatBytes(r.total_memory_bytes);
                document.getElementById("hw-swap-total").innerText = formatBytes(r.total_swap_bytes);
                document.getElementById("hw-storage-total").innerText = formatBytes(r.total_storage_bytes);

                document.getElementById("hw-gpu-status").innerText = data.has_gpu ? "DETECTED" : "None";
                document.getElementById("hw-gpu-model").innerText = data.gpu_details.join(", ") || "Standard";
                document.getElementById("hw-npu-status").innerText = data.has_npu ? "DETECTED" : "None";
                document.getElementById("hw-npu-model").innerText = data.npu_details.join(", ") || "None";
            } catch (err) {
                console.error("Failed to load hardware probe:", err);
            }
        }

        // Initial Load
        fetchHardware();
        fetchAccounting();
        window.addEventListener("resize", drawChart);
    </script>
</body>
</html>
"##;

pub fn create_web_router(handle: ArbiterHandle) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/status", get(status_handler))
        .route("/api/events", get(events_handler))
        .route("/api/accounting", get(accounting_handler))
        .route("/api/probe", get(probe_handler))
        .with_state(handle)
}

async fn index_handler() -> impl IntoResponse {
    Html(EMBEDDED_DASHBOARD_HTML)
}

async fn status_handler(State(handle): State<ArbiterHandle>) -> impl IntoResponse {
    let snap = handle.get_cached_snapshot().await;
    Json(snap)
}

async fn accounting_handler(
    State(handle): State<ArbiterHandle>,
    Query(params): Query<AccountingQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(100);
    let days = params.days.or(Some(30));
    match handle.send(ArbiterRequest::GetAccounting { limit, days }).await {
        Ok(ArbiterResponse::Accounting { summary, recent }) => {
            Json(serde_json::json!({
                "status": "ok",
                "summary": summary,
                "recent": recent
            }))
        }
        _ => Json(serde_json::json!({
            "status": "error",
            "error": "Failed to query accounting ledger"
        })),
    }
}

async fn probe_handler() -> impl IntoResponse {
    let hw = probe_host();
    Json(hw)
}

async fn events_handler(
    State(handle): State<ArbiterHandle>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
        Duration::from_millis(500),
    ))
    .then(move |_| {
        let h = handle.clone();
        async move {
            let snap = h.get_cached_snapshot().await;
            let json = serde_json::to_string(&snap).unwrap_or_default();
            Ok(Event::default().data(json))
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub async fn start_web_server(port: u16, handle: ArbiterHandle) -> Result<(), anyhow::Error> {
    let app = create_web_router(handle);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!("Nomos Web Dashboard live at http://localhost:{}", port);
    axum::serve(listener, app).await?;
    Ok(())
}
