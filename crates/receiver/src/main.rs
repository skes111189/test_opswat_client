use receiver::{router, Config, Service};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| "config/receiver.toml".into());
    let config: Config =
        toml::from_str(&std::fs::read_to_string(path).map_err(|_| "無法讀取 receiver 設定檔")?)
            .map_err(|_| "receiver 設定檔格式不正確")?;
    let listen = config.listen;
    let tls = config.tls.clone();
    let service = Service::new(config)?;
    let worker = tokio::spawn(service.clone().run());
    let app = router(service);
    if let Some(tls) = tls {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            tls.certificate_pem,
            tls.private_key_pem,
        )
        .await?;
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
        });
        axum_server::bind_rustls(listen, tls)
            .handle(handle)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
    }
    worker.abort();
    Ok(())
}
