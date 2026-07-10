use anyhow::{Context, Result};
use axum::{
    extract::{ConnectInfo, Request},
    http::StatusCode,
    Json, Router,
    middleware::{self, Next},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub async fn run(addr: &str, shutdown: CancellationToken) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/clear-stats", post(clear_stats))
        .layer(middleware::from_fn(require_loopback));

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind web admin server on {addr}"))?;
    tracing::info!("web admin server listening on http://{addr}");

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown.cancelled().await;
        })
        .await
        .context("web admin server failed")
}

async fn require_loopback(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    if addr.ip().is_loopback() {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Audio Link</title>
  <style>
    :root { color-scheme: light dark; font-family: "Segoe UI", system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: Canvas; color: CanvasText; }
    main { width: min(860px, calc(100vw - 40px)); }
    h1 { margin: 0 0 12px; font-size: 32px; font-weight: 650; }
    p { margin: 0 0 20px; color: color-mix(in srgb, CanvasText 72%, transparent); line-height: 1.6; }
    section { margin-top: 18px; }
    h2 { margin: 0 0 8px; font-size: 18px; font-weight: 650; }
    .section-title { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
    button { font: inherit; padding: 6px 12px; border: 1px solid color-mix(in srgb, CanvasText 24%, transparent); border-radius: 6px; background: ButtonFace; color: ButtonText; cursor: pointer; }
    button:disabled { cursor: default; opacity: 0.6; }
    dl { display: grid; grid-template-columns: max-content 1fr; gap: 10px 18px; padding: 18px; border: 1px solid color-mix(in srgb, CanvasText 18%, transparent); border-radius: 8px; }
    dt { color: color-mix(in srgb, CanvasText 64%, transparent); }
    dd { margin: 0; font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }
  </style>
</head>
<body>
  <main>
    <h1>Audio Link</h1>
    <p>本地管理页已经启动。这里会实时显示音频发送状态、诊断指标和 WASAPI 音频引擎周期。</p>

    <section>
      <h2>服务状态</h2>
      <dl>
        <dt>服务状态</dt><dd id="state">读取中...</dd>
        <dt>控制端口</dt><dd>9091</dd>
        <dt>管理端口</dt><dd>19092</dd>
      </dl>
    </section>

    <section>
      <div class="section-title">
        <h2>发送诊断</h2>
        <button id="clear-stats" type="button">清空统计</button>
      </div>
      <dl>
        <dt>UDP 音频流</dt><dd id="audio-active">读取中...</dd>
        <dt>发送目标</dt><dd id="audio-target">-</dd>
        <dt>发包间隔</dt><dd id="send-gap">-</dd>
        <dt>最大发包间隔</dt><dd id="max-send-gap">-</dd>
        <dt>WASAPI 触发间隔</dt><dd id="wasapi-trigger-gap">-</dd>
        <dt>最大 WASAPI 触发间隔</dt><dd id="max-wasapi-trigger-gap">-</dd>
      </dl>
    </section>

    <section>
      <h2>WASAPI 引擎周期</h2>
      <dl>
        <dt>查询状态</dt><dd id="engine-status">读取中...</dd>
        <dt>混音采样率</dt><dd id="engine-sample-rate">-</dd>
        <dt>默认周期</dt><dd id="engine-default">-</dd>
        <dt>基础周期</dt><dd id="engine-fundamental">-</dd>
        <dt>最小周期</dt><dd id="engine-min">-</dd>
        <dt>最大周期</dt><dd id="engine-max">-</dd>
      </dl>
    </section>
  </main>

  <script>
    const setText = (selector, text) => {
      document.querySelector(selector).textContent = text;
    };

    const clearButton = document.querySelector('#clear-stats');

    const formatPeriod = (period) => {
      if (!period) return '-';
      return `${period.frames} 帧 / ${period.ms.toFixed(3)} ms`;
    };

    const refresh = () => {
      fetch('/api/status')
        .then((response) => response.json())
        .then((status) => {
          setText('#state', status.status);
          setText('#audio-active', status.audio.active ? '发送中' : '未发送');
          setText('#audio-target', status.audio.target_addr || '-');
          setText('#send-gap', `${status.audio.send_gap_ms.toFixed(2)} ms`);
          setText('#max-send-gap', `${status.audio.max_send_gap_ms.toFixed(2)} ms`);
          setText('#wasapi-trigger-gap', `${status.audio.wasapi_trigger_gap_ms.toFixed(2)} ms`);
          setText('#max-wasapi-trigger-gap', `${status.audio.max_wasapi_trigger_gap_ms.toFixed(2)} ms`);

          const engine = status.audio_engine;
          setText('#engine-status', engine.supported ? '已获取' : (engine.error || '不支持'));
          setText('#engine-sample-rate', engine.sample_rate ? `${engine.sample_rate} Hz` : '-');
          setText('#engine-default', formatPeriod(engine.default_period));
          setText('#engine-fundamental', formatPeriod(engine.fundamental_period));
          setText('#engine-min', formatPeriod(engine.min_period));
          setText('#engine-max', formatPeriod(engine.max_period));
        })
        .catch(() => {
          setText('#state', 'unreachable');
          setText('#audio-active', '读取失败');
          setText('#engine-status', '读取失败');
        });
    };

    refresh();
    clearButton.addEventListener('click', () => {
      clearButton.disabled = true;
      fetch('/api/clear-stats', { method: 'POST' })
        .then(() => refresh())
        .finally(() => {
          clearButton.disabled = false;
        });
    });
    setInterval(refresh, 1000);
  </script>
</body>
</html>"#,
    )
}

async fn status() -> impl IntoResponse {
    let audio = crate::audio_stats::snapshot();
    let audio_engine = audio_engine_status_json();
    Json(serde_json::json!({
        "status": "running",
        "control_port": crate::audio_protocol::CONTROL_PORT,
        "web_admin": crate::app::WEB_ADMIN_ADDR,
        "audio": {
            "active": audio.active,
            "target_addr": audio.target_addr,
            "send_gap_ms": audio.send_gap_ms,
            "max_send_gap_ms": audio.max_send_gap_ms,
            "wasapi_trigger_gap_ms": audio.wasapi_trigger_gap_ms,
            "max_wasapi_trigger_gap_ms": audio.max_wasapi_trigger_gap_ms,

        },
        "audio_engine": audio_engine,
    }))
}

async fn clear_stats() -> impl IntoResponse {
    crate::audio_stats::clear_history();
    Json(serde_json::json!({ "ok": true }))
}

fn audio_engine_status_json() -> serde_json::Value {
    match crate::audio_engine_period::query_default_render_engine_period() {
        Ok(period) => serde_json::json!({
            "supported": true,
            "sample_rate": period.sample_rate,
            "default_period": period_json(period.default_frames, period.default_ms()),
            "fundamental_period": period_json(period.fundamental_frames, period.fundamental_ms()),
            "min_period": period_json(period.min_frames, period.min_ms()),
            "max_period": period_json(period.max_frames, period.max_ms()),
        }),
        Err(error) => serde_json::json!({
            "supported": false,
            "error": error.to_string(),
        }),
    }
}

fn period_json(frames: u32, ms: f64) -> serde_json::Value {
    serde_json::json!({
        "frames": frames,
        "ms": ms,
    })
}
