//! Единый тип ошибки проекта. Слои возвращают `bmv_common::Result<T>`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),

    #[error("код-приглашение: {0}")]
    Invite(String),

    #[error("конфиг: {0}")]
    Config(String),

    #[error("протокол: {0}")]
    Protocol(String),

    #[error("знакомство/сигналинг: {0}")]
    Signal(String),

    #[error("сеть/NAT: {0}")]
    Net(String),

    #[error("туннель: {0}")]
    Tunnel(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}
