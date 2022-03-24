use songbird::input::Input;

use self::netease::_netease;

mod netease;
mod encrypto;
use anyhow::Result;

pub(crate) async fn netease(url: &str) -> Result<Input> {
    Ok(_netease(url).await?)
}