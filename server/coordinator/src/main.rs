//! Бинарь координатора: env-конфиг + логи, вся логика — в библиотеке (lib.rs).

use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind: SocketAddr = std::env::var("BMV_COORD_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3330".into())
        .parse()
        .expect("BMV_COORD_BIND: неверный адрес");

    // TLS из env (standalone). Приоритет: свой сертификат → авто Let's Encrypt по
    // домену → HTTP. (Основной путь теперь — `bemyvpn server` из конфига.)
    let tls = match (std::env::var("BMV_TLS_CERT"), std::env::var("BMV_TLS_KEY")) {
        (Ok(c), Ok(k)) if !c.is_empty() && !k.is_empty() => bmv_coordinator::Tls::Files { cert: c, key: k },
        _ => match std::env::var("BMV_TLS_DOMAIN") {
            Ok(d) if !d.is_empty() => bmv_coordinator::Tls::Acme {
                domains: d.split(',').map(|s| s.trim().to_string()).collect(),
                email: std::env::var("BMV_TLS_EMAIL").ok().filter(|s| !s.is_empty()),
                cache: std::env::var("BMV_TLS_CACHE").unwrap_or_else(|_| "acme-cache".into()),
            },
            _ => bmv_coordinator::Tls::None,
        },
    };

    bmv_coordinator::serve(bind, tls, None, std::future::pending())
        .await
        .expect("координатор упал");
}
