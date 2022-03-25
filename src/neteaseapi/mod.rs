use songbird::input::{Input, Restartable};

use self::netease::{_netease, _netease_restartable};

mod netease;
mod encrypto;
use anyhow::Result;

pub(crate) async fn netease(url: &str) -> Result<Input> {
    _netease(url, None).await
}

pub(crate) async fn netease_restartable(url: &str, lazy: bool) -> Result<Restartable> {
    _netease_restartable(url, lazy).await
}