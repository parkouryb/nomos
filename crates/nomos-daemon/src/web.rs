use crate::actor::ArbiterHandle;
use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse,
    },
    routing::get,
    Json, Router,
};
use futures::stream::Stream;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::StreamExt as _;
use tracing::info;

const EMBEDDED_DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>NOMOS - Live Resource Arbiter Dashboard</title>
    <style>
        :root {
            --bg-primary: #0a0c10;
            --bg-secondary: #121620;
            --border: #232c3d;
            --text-primary: #f8fafc;
            --text-secondary: #94a3b8;
            --accent-blue: #3b82f6;
            --accent-green: #10b981;
            --accent-amber: #f59e0b;
            --accent-rose: #f43f5e;
            --font-mono: ui-monospace, SFMono-Regular, Menlo, monospace;
        }
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: var(--bg-primary); color: var(--text-primary); padding: 30px; }
        .container { max-width: 1100px; margin: 0 auto; }
        header { border-bottom: 1px solid var(--border); padding-bottom: 16px; margin-bottom: 24px; display: flex; justify-content: space-between; align-items: center; }
        h1 { font-size: 24px; font-weight: 700; color: #fff; font-family: var(--font-mono); }
        .badge { background: #1e293b; color: #38bdf8; font-size: 11px; padding: 4px 8px; border-radius: 4px; font-family: var(--font-mono); }
        .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 16px; margin-bottom: 24px; }
        .card { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 6px; padding: 16px; font-family: var(--font-mono); }
        .card-title { font-size: 11px; text-transform: uppercase; color: var(--text-secondary); margin-bottom: 6px; }
        .card-val { font-size: 20px; font-weight: 700; color: #fff; }
        .card-sub { font-size: 12px; color: #64748b; margin-top: 4px; }
        .progress-bg { background: #1e293b; border-radius: 4px; height: 8px; overflow: hidden; margin-top: 8px; }
        .progress-bar { height: 100%; background: var(--accent-blue); width: 0%; transition: width 0.3s ease; }
        table { width: 100%; border-collapse: collapse; background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 6px; overflow: hidden; font-family: var(--font-mono); font-size: 12.5px; margin-top: 16px; }
        th, td { padding: 10px 14px; text-align: left; border-bottom: 1px solid var(--border); }
        th { background: #182030; color: #cbd5e1; font-size: 11px; text-transform: uppercase; }
        .tag-running { background: #064e3b; color: #6ee7b7; padding: 2px 6px; border-radius: 3px; font-size: 11px; }
        .tag-queued { background: #78350f; color: #fde68a; padding: 2px 6px; border-radius: 3px; font-size: 11px; }
    </style>
</head>
<body>
    <div class="container">
        <header>
            <div>
                <h1>NOMOS ARBITER LIVE MONITOR</h1>
                <div style="font-size: 13px; color: var(--text-secondary); margin-top: 4px;">Single-Host Spatio-Temporal Resource Authority</div>
            </div>
            <div class="badge" id="conn-status">LIVE SSE CONNECTED</div>
        </header>

        <div class="grid">
            <div class="card">
                <div class="card-title">Nomos CPU Budget</div>
                <div class="card-val" id="cpu-val">0.0 / 0.0 Cores</div>
                <div class="progress-bg"><div class="progress-bar" id="cpu-bar"></div></div>
                <div class="card-sub" id="cpu-pct">0% Used</div>
            </div>
            <div class="card">
                <div class="card-title">Nomos RAM Budget</div>
                <div class="card-val" id="mem-val">0.0 / 0.0 GB</div>
                <div class="progress-bg"><div class="progress-bar" id="mem-bar" style="background: var(--accent-green);"></div></div>
                <div class="card-sub" id="mem-pct">0% Used</div>
            </div>
            <div class="card">
                <div class="card-title">Active / Queued Leases</div>
                <div class="card-val" id="leases-val">0 Active</div>
                <div class="card-sub" id="queued-sub">0 Queued</div>
            </div>
        </div>

        <h3 style="font-family: var(--font-mono); font-size: 14px; margin-top: 24px; color: #cbd5e1;">ACTIVE WORKER LEASES</h3>
        <table>
            <thead>
                <tr>
                    <th>Lease ID</th>
                    <th>Worker ID</th>
                    <th>CPU (vCPUs)</th>
                    <th>RAM</th>
                    <th>Priority</th>
                    <th>State</th>
                </tr>
            </thead>
            <tbody id="leases-tbody">
                <tr><td colspan="6" style="text-align: center; color: #64748b;">No active worker leases</td></tr>
            </tbody>
        </table>
    </div>

    <script>
        const evtSource = new EventSource("/api/events");
        evtSource.onmessage = function(event) {
            const data = JSON.parse(event.data);
            const pool = data.pool;

            const cpuUsed = data.allocated_cpu;
            const cpuTotal = pool.total_cores;
            const cpuPct = cpuTotal > 0 ? (cpuUsed / cpuTotal * 100).toFixed(1) : 0;
            document.getElementById("cpu-val").innerText = `${cpuUsed.toFixed(1)} / ${cpuTotal.toFixed(1)} Cores`;
            document.getElementById("cpu-bar").style.width = `${Math.min(cpuPct, 100)}%`;
            document.getElementById("cpu-pct").innerText = `${cpuPct}% of Nomos Pool`;

            const memUsedGB = (data.allocated_memory_bytes / (1024**3)).toFixed(2);
            const memTotalGB = (pool.total_memory_bytes / (1024**3)).toFixed(2);
            const memPct = pool.total_memory_bytes > 0 ? (data.allocated_memory_bytes / pool.total_memory_bytes * 100).toFixed(1) : 0;
            document.getElementById("mem-val").innerText = `${memUsedGB} / ${memTotalGB} GiB`;
            document.getElementById("mem-bar").style.width = `${Math.min(memPct, 100)}%`;
            document.getElementById("mem-pct").innerText = `${memPct}% of Nomos Pool`;

            document.getElementById("leases-val").innerText = `${data.active_leases.length} Active`;
            document.getElementById("queued-sub").innerText = `${data.queued_leases.length} Waiting in queue`;

            const tbody = document.getElementById("leases-tbody");
            if (data.active_leases.length === 0) {
                tbody.innerHTML = '<tr><td colspan="6" style="text-align: center; color: #64748b;">No active worker leases</td></tr>';
            } else {
                tbody.innerHTML = data.active_leases.map(l => `
                    <tr>
                        <td style="color: #38bdf8;">${l.id}</td>
                        <td>${l.request.worker_id}</td>
                        <td>${l.request.req_cpu.toFixed(1)}</td>
                        <td>${(l.request.req_memory_bytes / (1024**3)).toFixed(2)} GiB</td>
                        <td>${l.request.priority}</td>
                        <td><span class="tag-running">${l.state}</span></td>
                    </tr>
                `).join("");
            }
        };
        evtSource.onerror = function() {
            document.getElementById("conn-status").innerText = "RECONNECTING...";
            document.getElementById("conn-status").style.color = "#f59e0b";
        };
    </script>
</body>
</html>
"##;

pub fn create_web_router(handle: ArbiterHandle) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/status", get(status_handler))
        .route("/api/events", get(events_handler))
        .with_state(handle)
}

async fn index_handler() -> impl IntoResponse {
    Html(EMBEDDED_DASHBOARD_HTML)
}

async fn status_handler(State(handle): State<ArbiterHandle>) -> impl IntoResponse {
    let snap = handle.get_cached_snapshot().await;
    Json(snap)
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
