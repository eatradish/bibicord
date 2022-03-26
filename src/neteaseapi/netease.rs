use std::{
    collections::HashMap,
    io::ErrorKind,
    process::{Command, Stdio},
    time::Duration,
};

use crate::neteaseapi::encrypto::Crypto;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::{Client, Response, Url};
use serde::{Deserialize, Serialize};
use songbird::input::{
    children_to_reader, restartable::Restart, Codec, Container, Input, Metadata, Restartable,
};
use tracing::{debug, info};

#[derive(Deserialize, Serialize)]
struct SongResult {
    data: Vec<SongDataResult>,
    code: usize,
}

#[derive(Deserialize, Serialize)]
struct SongDataResult {
    url: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct Ids {
    id: String,
}

#[derive(Deserialize, Serialize)]
struct SongDetailRequest {
    c: Vec<Ids>,
    ids: Vec<u64>,
}
struct NeteaseClient {
    client: Client,
}

#[derive(Deserialize, Debug)]
struct SongDetailResult {
    songs: Vec<SongDetailSong>,
}

#[derive(Deserialize, Debug)]
struct SongDetailSong {
    name: Option<String>,
    #[serde(default)]
    artists: Vec<SongDetailSongArtist>,
    duration: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct SongDetailSongArtist {
    name: Option<String>,
}

#[derive(Deserialize, Debug)]
struct DjDetail {
    program: Option<DjDetailProgram>,
}

#[derive(Deserialize, Debug)]
struct DjDetailProgram {
    #[serde(rename(deserialize = "mainSong"))]
    main_song: Option<DjDetailProgramMainSong>,
}

#[derive(Deserialize, Debug)]
struct DjDetailProgramMainSong {
    name: Option<String>,
    id: Option<u64>,
    #[serde(default)]
    artists: Vec<SongDetailSongArtist>,
    duration: Option<u64>,
}

enum NeteaseTyoe {
    Normal,
    Dj,
}

const USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 9_1 like Mac OS X) AppleWebKit/601.1.46 (KHTML, like Gecko) Version/9.0 Mobile/13B143 Safari/601.1";
const BASE_URL: &str = "https://music.163.com/weapi";

impl From<&SongDetailSong> for Metadata {
    fn from(song: &SongDetailSong) -> Self {
        let artists = artist_trans(&song.artists);
        let duration = song.duration.map(Duration::from_millis);

        Self {
            track: None,
            artist: Some(artists),
            date: None,
            channels: Some(2),
            channel: None,
            start_time: None,
            duration,
            sample_rate: Some(48000),
            source_url: None,
            title: song.name.to_owned(),
            thumbnail: None,
        }
    }
}

impl From<&DjDetailProgramMainSong> for Metadata {
    fn from(song: &DjDetailProgramMainSong) -> Self {
        let artists = artist_trans(&song.artists);
        let duration = song.duration.map(Duration::from_millis);

        Self {
            track: None,
            artist: Some(artists),
            date: None,
            channels: Some(2),
            channel: None,
            start_time: None,
            duration,
            sample_rate: Some(48000),
            source_url: None,
            title: song.name.to_owned(),
            thumbnail: None,
        }
    }
}

fn artist_trans(artists: &[SongDetailSongArtist]) -> String {
    let artists = artists
        .iter()
        .filter_map(|x| x.name.clone())
        .collect::<Vec<_>>()
        .join(", ");

    artists
}

impl NeteaseClient {
    fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self { client })
    }

    async fn post(&self, url: &str, params: HashMap<&str, &str>) -> Result<Response> {
        let params = crypto_params(params)?;

        Ok(self.client.post(url).query(&params).send().await?)
    }
}

struct NeteaseRestarter {
    url: String,
    client: NeteaseClient,
}

impl NeteaseRestarter {
    fn new(url: &str, client: NeteaseClient) -> Self {
        Self {
            url: url.to_string(),
            client,
        }
    }
}

#[async_trait]
impl Restart for NeteaseRestarter {
    async fn call_restart(
        &mut self,
        time: Option<Duration>,
    ) -> songbird::input::error::Result<Input> {
        Ok(_netease(&self.url, time)
            .await
            .map_err(|e| std::io::Error::new(ErrorKind::Other, e))?)
    }

    async fn lazy_init(
        &mut self,
    ) -> songbird::input::error::Result<(Option<Metadata>, Codec, Container)> {
        let url = &self.url;
        let t = if url.contains("program") {
            NeteaseTyoe::Dj
        } else {
            NeteaseTyoe::Normal
        };

        let metadata = match t {
            NeteaseTyoe::Normal => get_song_metadata(
                &self.client,
                &[get_music_id(url).map_err(|e| std::io::Error::new(ErrorKind::Other, e))?],
            )
            .await
            .map_err(|e| std::io::Error::new(ErrorKind::Other, e))?,
            NeteaseTyoe::Dj => {
                get_dj_music_url_and_detail(&self.client, url)
                    .await
                    .map_err(|e| std::io::Error::new(ErrorKind::Other, e))?
                    .1
            }
        };

        Ok((Some(metadata), Codec::FloatPcm, Container::Raw))
    }
}

pub async fn _netease_restartable(url: &str, lazy: bool) -> Result<Restartable> {
    let client = NeteaseClient::new()?;

    Ok(Restartable::new(NeteaseRestarter::new(url, client), lazy).await?)
}

fn crypto_params(params: HashMap<&str, &str>) -> Result<Vec<(String, String)>> {
    let params = serde_json::to_string(&params)?;
    let params = Crypto::weapi(&params);

    Ok(params)
}

async fn get_song_url(client: &NeteaseClient, ids: &[u64]) -> Result<Vec<String>> {
    let url = format!("{}/song/enhance/player/url/", BASE_URL);
    let ids = serde_json::to_string(ids)?;
    let mut params = HashMap::new();
    params.insert("ids", &ids[..]);
    params.insert("br", "320000");
    let song_result = client
        .post(&url, params)
        .await?
        .json::<SongResult>()
        .await?;
    let urls = song_result
        .data
        .into_iter()
        .map(|x| x.url)
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(anyhow!("Url list is empty!"));
    }

    Ok(urls)
}

async fn get_song_metadata(client: &NeteaseClient, ids: &[u64]) -> Result<Metadata> {
    let url = format!("{}/song/detail", BASE_URL);
    let c = ids
        .iter()
        .map(|x| Ids { id: x.to_string() })
        .collect::<Vec<_>>();
    let c = serde_json::to_string(&c)?;
    let ids = ids.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let ids = serde_json::to_string(&ids)?;
    let mut params = HashMap::new();
    params.insert("c", &c[..]);
    params.insert("ids", &ids[..]);
    let result = client
        .post(&url, params)
        .await?
        .json::<SongDetailResult>()
        .await?;
    debug!("{:?}", result);
    let result = result
        .songs
        .first()
        .ok_or_else(|| anyhow!("Can not get song list!"))?;
    let result = Metadata::from(result);

    Ok(result)
}

fn get_music_id(url: &str) -> Result<u64> {
    let url = url.replace("/#", "");
    let url = Url::parse(&url)?;
    let parms = url.query().ok_or_else(|| anyhow!("Url is not right!"))?;
    let id = parms
        .split('&')
        .find(|x| x.starts_with("id="))
        .ok_or_else(|| anyhow!("Url is not right!"))?
        .strip_prefix("id=")
        .unwrap()
        .parse::<u64>()?;

    Ok(id)
}

async fn get_dj_music_url_and_detail(client: &NeteaseClient, url: &str) -> Result<(String, Metadata)> {
    let dj_id = get_music_id(url)?.to_string();
    let url = format!("{}/{}", BASE_URL, "/dj/program/detail");
    let mut params = HashMap::new();
    params.insert("id", dj_id.as_str());
    let dj_detail = client.post(&url, params).await?.json::<DjDetail>().await?;
    let main_song = dj_detail
        .program
        .as_ref()
        .and_then(|x| x.main_song.as_ref());
    let id = main_song.and_then(|x| x.id);
    let id = id.ok_or_else(|| anyhow!("Can not get song id from dj detail!"))?;
    let song_url = get_song_url(&client, &[id]).await?;
    let metadata = Metadata::from(main_song.ok_or_else(|| anyhow!("Can not get metadata!"))?);
    debug!("{:?}", metadata);

    Ok((song_url[0].to_owned(), metadata))
}

pub(crate) async fn _netease(uri: &str, time: Option<Duration>) -> Result<Input> {
    let client = NeteaseClient::new()?;
    dbg!(uri);
    let t = if uri.contains("program") {
        NeteaseTyoe::Dj
    } else {
        NeteaseTyoe::Normal
    };
    let (url, metadata) = match t {
        NeteaseTyoe::Dj => get_dj_music_url_and_detail(&client, uri).await?,
        NeteaseTyoe::Normal => {
            let id = get_music_id(uri)?;
            let urls = get_song_url(&client, &[id]).await?;
            let url = urls[0].to_owned();
            let metadata = get_song_metadata(&client, &[id]).await?;

            (url, metadata)
        }
    };
    let time = time.unwrap_or_else(|| Duration::from_secs(0));
    let time = format!("{:.3}", time.as_secs_f64());
    let from_pipe_args = vec![
        "-ss",
        time.as_str(),
        "-i",
        url.as_str(),
        "-acodec",
        "pcm_f32le",
        "-ac",
        "2",
        "-ar",
        "48000",
        "-f",
        "s16le",
        "-",
    ];

    let from_pipe_command = Command::new("ffmpeg")
        .args(from_pipe_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    info!("netease music metadata {:?}", metadata);

    Ok(Input::new(
        true,
        children_to_reader::<f32>(vec![from_pipe_command]),
        Codec::FloatPcm,
        Container::Raw,
        Some(metadata),
    ))
}

#[test]
fn test_get_music_id() {
    let url = "https://music.163.com/#/song?id=26209670";
    let id = get_music_id(url).unwrap();

    assert_eq!(id, 26209670);
}

#[tokio::test]
async fn test_get_song_url() {
    let client = NeteaseClient::new().unwrap();
    let url = get_song_url(&client, &[26209670]).await.unwrap();
    let filename = url[0].split('/').last().unwrap();

    assert_eq!(filename, "fa0240b65deaf3360c8812c629fe1820.mp3");
}

#[tokio::test]
async fn test_get_song_detail() {
    let client = NeteaseClient::new().unwrap();
    let _ = get_song_metadata(&client, &[26209670]).await.unwrap();
}

#[tokio::test]
async fn test_get_dj_detail() {
    let client = NeteaseClient::new().unwrap();
    let url = "https://music.163.com/#/program?id=2493262449";
    let (song_url, metadata) = get_dj_music_url_and_detail(&client, url).await.unwrap();

    assert_eq!(
        song_url.split('/').last().unwrap(),
        "19716f882ebc8a95bc2abdfe346268c7.mp3"
    );
    assert_eq!(metadata.title.unwrap(), "原来你什么都不想要");
}
