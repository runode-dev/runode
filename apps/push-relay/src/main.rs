//! `runode-push` 按环境变量起服务；`runode-push keygen` 打印一把新的句柄密钥。
//!
//! 环境变量：`RELAY_HANDLE_KEYS`（`handle::Keys::parse` 的格式）、`APNS_KEY_FILE`、`APNS_KEY_ID`、
//! `APNS_TEAM_ID` 必填；`RELAY_LISTEN` 是监听地址，缺省时是 `DEFAULT_LISTEN`；`RELAY_TRUST_PROXY=1` 时
//! 打开 `Relay::new` 的 `trust_proxy`。日志级别照 `RUST_LOG`，默认 `info`。

use std::{env, net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc};

use runode_push::{
    apns::Client,
    handle::{Keys, generate_key},
    server::{Relay, router},
};
use tracing_subscriber::EnvFilter;

const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() -> ExitCode {
    if env::args().nth(1).as_deref() == Some("keygen") {
        return keygen();
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    match serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            tracing::error!("{why}");
            ExitCode::FAILURE
        }
    }
}

#[expect(clippy::print_stdout, clippy::print_stderr, reason = "密钥打到标准输出，给人抄进配置")]
fn keygen() -> ExitCode {
    match generate_key() {
        Ok(key) => {
            println!("{key}");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("the system random number generator failed");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), String> {
    let var = |name: &str| env::var(name).map_err(|_| format!("{name} is not set"));
    let keys = Keys::parse(&var("RELAY_HANDLE_KEYS")?)?;
    let apns = Client::new(&PathBuf::from(var("APNS_KEY_FILE")?), &var("APNS_KEY_ID")?, &var("APNS_TEAM_ID")?)?;
    let trust_proxy = env::var("RELAY_TRUST_PROXY").is_ok_and(|value| value == "1");
    let listen = env::var("RELAY_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_owned());
    let listener =
        tokio::net::TcpListener::bind(&listen).await.map_err(|err| format!("cannot listen on {listen}: {err}"))?;
    tracing::info!("listening on {listen}");
    let app = router(Arc::new(Relay::new(keys, apns, trust_proxy)));
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|err| format!("the server stopped: {err}"))
}

/// Ctrl-C 或 systemd 停服务时发的 SIGTERM；收不到信号时一直等着，不当作要停。
async fn shutdown() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        #[cfg(unix)]
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
            return;
        }
        std::future::pending::<()>().await;
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
