//! Example demonstrating how to make use of individual track audio events,
//! and how to use the `TrackQueue` system.
//!
//! Requires the "cache", "standard_framework", and "voice" features be enabled in your
//! Cargo.toml, like so:
//!
//! ```toml
//! [dependencies.serenity]
//! git = "https://github.com/serenity-rs/serenity.git"
//! features = ["cache", "framework", "standard_framework", "voice"]
//! ```
use std::{collections::HashMap, env, sync::Arc, time::Duration};

mod neteaseapi;

use serenity::{
    async_trait,
    client::{Client, Context, EventHandler},
    framework::{
        standard::{
            macros::{command, group},
            Args, CommandResult,
        },
        StandardFramework,
    },
    http::Http,
    model::{channel::Message, gateway::Ready, prelude::ChannelId},
    prelude::{TypeMapKey, GatewayIntents, Mentionable},
    Result as SerenityResult,
};

use songbird::{
    input::{self, restartable::Restartable},
    Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit, TrackEvent,
};

use anyhow::anyhow;
use tokio::sync::RwLock;

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[group]
#[commands(
    deafen, join, leave, mute, play_fade, play, skip, clear, ping, undeafen, unmute, list, destroy,
    now, vol, help
)]
struct General;

struct SongVolume;

impl TypeMapKey for SongVolume {
    type Value = Arc<RwLock<HashMap<u64, f32>>>;
}

macro_rules! unwrap_or_show_error {
    ($f:expr, $msg:ident, $ctx:ident) => {
        match $f {
            Ok(source) => source,
            Err(why) => {
                println!("Err starting source: {:?}", why);
                check_msg(
                    $msg.channel_id
                        .say(&$ctx.http, "Error sourcing ffmpeg")
                        .await,
                );

                return Ok(());
            }
        }
    };
}

const DEP_APP_LIST: &[&str] = &["ffmpeg", "ffprobe", "youtube-dl"];

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();

    for app in DEP_APP_LIST {
        if which::which(app).is_err() {
            eprintln!("Can not find {} in PATH!", app);
            std::process::exit(1);
        }
    }
    tracing_subscriber::fmt::init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let framework = StandardFramework::new()
        .configure(|c| c.prefix("~"))
        .group(&GENERAL_GROUP);

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Err creating client");

    {
        // Open the data lock in write mode, so keys can be inserted to it.
        let mut data = client.data.write().await;

        // The CommandCounter Value has the following type:
        // Arc<RwLock<HashMap<String, u64>>>
        // So, we have to insert the same type to it.
        data.insert::<SongVolume>(Arc::new(RwLock::new(HashMap::default())));
    }

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}

#[command]
async fn deafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let handler_lock = match manager.get(guild_id) {
        Some(handler) => handler,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let mut handler = handler_lock.lock().await;

    if handler.is_deaf() {
        check_msg(msg.channel_id.say(&ctx.http, "Already deafened").await);
    } else {
        if let Err(e) = handler.deafen(true).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Deafened").await);
    }

    Ok(())
}

fn duration_formatter(duration: &Duration) -> String {
    let seconds = duration.as_secs();

    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

#[command]
#[only_in(guilds)]
async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    let help = r#"Usage:
~join             join to voice channel
~play [URL]       play audio from URL
~now              See now playing
~list             See current audio queue
~clean            Clean current audio queue
~destroy          Clean current audio queue and leave
~leave            Leave voice channel
~vol [VOL]        Set volume (0~200)
"#;
    check_msg(msg.channel_id.say(&ctx.http, help).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let channel_id = guild
        .voice_states
        .get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let (_, success) = manager.join(guild_id, connect_to).await;

    if let Ok(_channel) = success {
        check_msg(
            msg.channel_id
                .say(&ctx.http, &format!("Joined {}", connect_to.mention()))
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Error joining the channel")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();
    let has_handler = manager.get(guild_id).is_some();

    let song_volume_lock = {
        let read = ctx.data.read().await;

        read.get::<SongVolume>()
            .expect("Expected SongVolume in TypeMap.")
            .clone()
    };

    {
        let mut song_volume = song_volume_lock.write().await;
        song_volume.remove(&msg.channel_id.0);
    }

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Left voice channel").await);
    } else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn mute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let handler_lock = match manager.get(guild_id) {
        Some(handler) => handler,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let mut handler = handler_lock.lock().await;

    if handler.is_mute() {
        check_msg(msg.channel_id.say(&ctx.http, "Already muted").await);
    } else {
        if let Err(e) = handler.mute(true).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Now muted").await);
    }

    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.channel_id.say(&ctx.http, "Pong!").await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play_fade(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let url = match args.single::<String>() {
        Ok(url) => url,
        Err(_) => {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, "Must provide a URL to a video or audio")
                    .await,
            );

            return Ok(());
        }
    };

    if !url.starts_with("http") {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Must provide a valid URL")
                .await,
        );

        return Ok(());
    }

    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        let t = if url.contains("music.163.com") {
            SourceType::Netease
        } else {
            SourceType::Ytdl
        };

        let source = match t {
            SourceType::Netease => unwrap_or_show_error!(neteaseapi::netease(&url).await, msg, ctx),
            SourceType::Ytdl => unwrap_or_show_error!(input::ytdl(&url).await, msg, ctx),
        };

        // This handler object will allow you to, as needed,
        // control the audio track via events and further commands.
        let song = handler.play_source(source);
        let send_http = ctx.http.clone();
        let chan_id = msg.channel_id;

        // This shows how to periodically fire an event, in this case to
        // periodically make a track quieter until it can be no longer heard.
        let _ = song.add_event(
            Event::Periodic(Duration::from_secs(5), Some(Duration::from_secs(7))),
            SongFader {
                chan_id,
                http: send_http,
            },
        );

        let send_http = ctx.http.clone();

        // This shows how to fire an event once an audio track completes,
        // either due to hitting the end of the bytestream or stopped by user code.
        let _ = song.add_event(
            Event::Track(TrackEvent::End),
            SongEndNotifier {
                chan_id,
                http: send_http,
            },
        );

        check_msg(msg.channel_id.say(&ctx.http, "Playing song").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

struct SongFader {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongFader {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(&[(state, track)]) = ctx {
            let _ = track.set_volume(state.volume / 2.0);

            if state.volume < 1e-2 {
                let _ = track.stop();
                check_msg(self.chan_id.say(&self.http, "Stopping song...").await);
                Some(Event::Cancel)
            } else {
                check_msg(self.chan_id.say(&self.http, "Volume reduced.").await);
                None
            }
        } else {
            None
        }
    }
}

struct SongEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        check_msg(
            self.chan_id
                .say(&self.http, "Song faded out completely!")
                .await,
        );

        None
    }
}

enum SourceType {
    Ytdl,
    Netease,
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let url = match args.single::<String>() {
        Ok(url) => url,
        Err(_) => {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, "Must provide a URL to a video or audio")
                    .await,
            );

            return Ok(());
        }
    };

    if !url.starts_with("http") {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Must provide a valid URL")
                .await,
        );

        return Ok(());
    }

    let song_volume_lock = {
        let read = ctx.data.read().await;

        read.get::<SongVolume>()
            .expect("Expected SongVolume in TypeMap.")
            .clone()
    };

    let volume = {
        let mut song_volume = song_volume_lock.write().await;
        let entry = song_volume
            .entry(msg.channel_id.0)
            .or_insert(1.0)
            .to_owned();

        entry
    };

    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        let t = if url.contains("music.163.com") {
            SourceType::Netease
        } else {
            SourceType::Ytdl
        };

        // Here, we use lazy restartable sources to make sure that we don't pay
        // for decoding, playback on tracks which aren't actually live yet.
        let source = match t {
            SourceType::Ytdl => unwrap_or_show_error!(Restartable::ytdl(url, true).await, msg, ctx),
            SourceType::Netease => {
                unwrap_or_show_error!(neteaseapi::netease_restartable(&url, true).await, msg, ctx)
            }
        };

        handler.enqueue_source(source.into());
        let queue = handler.queue().current_queue();
        let last = queue.last().ok_or_else(|| anyhow!("Can not get last!"))?;
        last.set_volume(volume)?;
        let metadata = last.metadata();
        let title = metadata.title.clone();
        let url = metadata.source_url.clone();
        let s = if let Some(title) = title {
            title
        } else if let Some(url) = url {
            url
        } else {
            "song".to_string()
        };

        check_msg(
            msg.channel_id
                .say(&ctx.http, format!("Added {} to queue", s))
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn skip(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        if args.is_empty() {
            let _ = queue.skip();
        } else if let Ok(index) = args.single::<usize>() {
            if index < 1 || index > queue.current_queue().len() {
                check_msg(
                    msg.channel_id
                        .say(&ctx.http, "Index must 1 to queue length!".to_string())
                        .await,
                )
            } else {
                queue.dequeue(index - 1);
            }
        }

        check_msg(
            msg.channel_id
                .say(
                    &ctx.http,
                    format!("Song skipped: {} in queue.", queue.len()),
                )
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn clear(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        let _ = queue.stop();

        check_msg(msg.channel_id.say(&ctx.http, "Queue cleared.").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn destroy(ctx: &Context, msg: &Message, args: Args) -> CommandResult {
    clear(ctx, msg, args.clone()).await?;
    leave(ctx, msg, args).await?;

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn now(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let list = handler.queue().current_queue();
        if list.last().is_none() {
            check_msg(msg.channel_id.say(&ctx.http, "List is empty!").await);
        }
        let metadata = list.first().unwrap().metadata();
        let title = metadata.title.as_ref();
        let artist = metadata.artist.as_ref();
        let url = metadata.source_url.as_ref();
        let duration = metadata.duration.as_ref();
        let mut s = String::from("Now Playing:\n");
        if let Some(title) = title {
            s.push_str(&format!("{}\n", title));
        }
        if let Some(artist) = artist {
            s.push_str(&format!("{}\n", artist));
        }
        if let Some(url) = url {
            s.push_str(&format!("{}\n", url))
        }
        if let Some(duration) = duration {
            s.push_str(&format!("{}\n", duration_formatter(duration)))
        }
        check_msg(msg.channel_id.say(&ctx.http, s).await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn vol(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();
    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let list = handler.queue().current_queue();
        if args.is_empty() {
            let entry = list.first();
            match entry {
                Some(entry) => {
                    let vol = entry.get_info().await?.volume;
                    check_msg(
                        msg.channel_id
                            .say(&ctx.http, format!("Volume is {:.0}", (vol * 100.0).round()))
                            .await,
                    );
                }
                None => {
                    check_msg(msg.channel_id.say(&ctx.http, "Queue is empty!").await);
                }
            }

            return Ok(());
        }
        let s = args.parse::<String>()?;
        if s.to_lowercase().contains('e') || s.contains('-') || s.contains('+') {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, "你他妈故意找茬是不是？你设不设音量吧？")
                    .await,
            );
        }
        let vol = args.single::<f32>();
        if let Ok(vol) = vol {
            if !(0.0..=200.0).contains(&vol) {
                check_msg(
                    msg.channel_id
                        .say(&ctx.http, "Volume must in 0 ~ 200")
                        .await,
                );

                return Ok(());
            }
            let vol = vol / 100.0;
            let song_volume_lock = {
                let read = ctx.data.read().await;

                read.get::<SongVolume>()
                    .expect("Expected SongVolume in TypeMap.")
                    .clone()
            };
            {
                let mut song_volume = song_volume_lock.write().await;
                let entry = song_volume.entry(msg.channel_id.0).or_insert(1.0);

                *entry = vol;
            }
            for i in list {
                i.set_volume(vol)?;
            }
            check_msg(
                msg.channel_id
                    .say(
                        &ctx.http,
                        format!("Volume set to {:.0}", (vol * 100.0).round()),
                    )
                    .await,
            );
        } else {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, "Volume must in 0 ~ 200")
                    .await,
            );
        }
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn list(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();
    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        let list = queue.current_queue();
        let mut s = String::new();
        for (i, c) in list.iter().enumerate() {
            let time = &c.metadata().duration;
            if let Some(title) = &c.metadata().title {
                s.push_str(&format!("{}. {}", i + 1, title));
            } else if let Some(url) = &c.metadata().source_url {
                s.push_str(&format!("{}. {}", i + 1, url));
            }
            if let Some(t) = time {
                s.push_str(&format!(" {}", duration_formatter(t)));
            }
            s.push('\n');
        }
        if !s.is_empty() {
            check_msg(msg.channel_id.say(&ctx.http, s).await);
        } else {
            check_msg(msg.channel_id.say(&ctx.http, "List is empty!").await)
        }
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn undeafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        if let Err(e) = handler.deafen(false).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Undeafened").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to undeafen in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn unmute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        if let Err(e) = handler.mute(false).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Unmuted").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to unmute in")
                .await,
        );
    }

    Ok(())
}

/// Checks that a message successfully sent; if not, then logs why to stdout.
fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        println!("Error sending message: {:?}", why);
    }
}
