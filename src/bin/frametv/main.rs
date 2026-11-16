//! `frametv`: manage the art on your Samsung Frame TVs.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use samsung_frame::art::{ArtClient, ArtItem};
use samsung_frame::config::{self, Config, TvConfig};
use samsung_frame::image_prep::{Fit, PrepOptions};
use samsung_frame::protocol::category;
use samsung_frame::sync::{self, SyncEvent, TvState, UploadSettings};
use samsung_frame::{discovery, pairing, remote, rest, wol};

mod api;
mod serve;

#[derive(Parser)]
#[command(name = "frametv", version, about = "Manage the art on your Samsung Frame TVs", arg_required_else_help = true)]
struct Cli {
    /// TV(s) to act on, by name or IP (repeatable). Defaults to the default TV, or the only one.
    #[arg(long = "tv", short = 't', global = true)]
    tvs: Vec<String>,
    /// Act on every configured TV.
    #[arg(long, global = true, conflicts_with = "tvs")]
    all: bool,
    /// Send Wake-on-LAN first and wait for the TV to come up.
    #[arg(long, global = true)]
    wake: bool,
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Config file (default: per-user config dir; env FRAMETV_CONFIG).
    #[arg(long, global = true, env = "FRAMETV_CONFIG")]
    config: Option<PathBuf>,
    /// More logging (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find Samsung TVs on the network.
    Discover {
        /// Also probe every address on the local /24 and on a.b.0-5.* (slower; for networks that block SSDP).
        #[arg(long)]
        scan: bool,
        /// Seconds to wait for SSDP replies.
        #[arg(long, default_value_t = 3)]
        wait: u64,
    },
    /// Pair with a TV (accept the prompt on the TV) and remember it.
    Pair {
        host: String,
        /// Name to refer to it by (default: the TV's own name).
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 8002)]
        port: u16,
        /// Make it the default TV.
        #[arg(long)]
        default: bool,
    },
    /// List configured TVs.
    Tvs,
    /// Change a configured TV's settings.
    Configure {
        tv: String,
        /// Folder that `sync` mirrors to this TV.
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Default matte (`auto`, `none`, `shadowbox_polar`, ... see `mattes`).
        #[arg(long)]
        matte: Option<String>,
        #[arg(long)]
        portrait_matte: Option<String>,
        /// Default fit: auto, cover, contain, original.
        #[arg(long)]
        fit: Option<Fit>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        mac: Option<String>,
        #[arg(long)]
        rename: Option<String>,
        /// Make it the default TV.
        #[arg(long)]
        default: bool,
    },
    /// Remove a TV from the config.
    Forget { tv: String },
    /// Show power, art mode, current artwork and slideshow state.
    Status,
    /// List artwork on the TV.
    List {
        /// Category: my-photos (default), favorites, store, all, or a raw id like MY-C0002.
        #[arg(long, short, default_value = "my-photos")]
        category: String,
    },
    /// Show the artwork currently displayed.
    Current,
    /// Upload images (prepared to fit the screen) to My Photos.
    Upload {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// auto, cover, contain, original.
        #[arg(long)]
        fit: Option<Fit>,
        /// Matte id or `auto` (none when it fills the screen, white shadowbox otherwise).
        #[arg(long)]
        matte: Option<String>,
        #[arg(long)]
        portrait_matte: Option<String>,
        /// Display the (last) uploaded image right away.
        #[arg(long)]
        show: bool,
    },
    /// Mirror a folder to My Photos (uploads new images; `--delete` removes ones you deleted).
    Sync {
        /// Folder (default: the TV's configured folder).
        folder: Option<PathBuf>,
        /// Delete images this tool uploaded whose source file is gone.
        #[arg(long)]
        delete: bool,
        /// Show what would happen without changing anything.
        #[arg(long, short = 'n')]
        dry_run: bool,
        #[arg(long)]
        fit: Option<Fit>,
        #[arg(long)]
        matte: Option<String>,
        #[arg(long)]
        portrait_matte: Option<String>,
    },
    /// Delete artwork from the TV.
    Delete {
        ids: Vec<String>,
        /// Delete everything this tool uploaded to the TV.
        #[arg(long, conflicts_with = "ids")]
        all_mine: bool,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Display an artwork.
    Show {
        id: String,
        /// Select without switching the screen to it.
        #[arg(long)]
        quiet: bool,
    },
    /// Turn art mode on/off or query it.
    Artmode { state: Option<OnOff> },
    /// Configure the slideshow: `off` or minutes between images.
    Slideshow {
        /// `off` or minutes (e.g. 15, 60, 1440). Omit to show the current setting.
        interval: Option<String>,
        /// Play in order instead of shuffling.
        #[arg(long)]
        sequential: bool,
        #[arg(long, short, default_value = "my-photos")]
        category: String,
    },
    /// List available mattes.
    Mattes,
    /// Change an artwork's matte.
    Matte {
        id: String,
        matte: String,
        #[arg(long)]
        portrait: Option<String>,
    },
    /// List photo filters.
    Filters,
    /// Apply a photo filter to an artwork.
    Filter { id: String, filter: String },
    /// Show art mode settings (brightness, color temperature, motion sensor...).
    Settings,
    /// Change an art mode setting, e.g. `set brightness 5`.
    Set { item: String, value: String },
    /// Mark/unmark an artwork as a favourite.
    Favorite {
        id: String,
        #[arg(long)]
        remove: bool,
    },
    /// Download thumbnails.
    Thumbs {
        /// Content ids (default: everything in My Photos).
        ids: Vec<String>,
        #[arg(long, short, default_value = "thumbnails")]
        out: PathBuf,
    },
    /// Send Wake-on-LAN.
    Wake,
    /// Press a remote key (KEY_POWER, KEY_HOME, ...).
    Key {
        key: String,
        /// Hold for this many seconds instead of clicking.
        #[arg(long)]
        hold: Option<f32>,
    },
    /// Print art events as they happen.
    Watch,
    /// Send a raw art request, e.g. '{"request":"get_api_version"}'.
    Raw { request: String },
    /// Run the web app (browse, upload, manage; guests can send photos) and LocalSend targets.
    /// Everything else (admin password, share address, LocalSend) is set in the web app.
    Serve {
        /// Port for the web app.
        #[arg(long, default_value_t = 3273)]
        port: u16,
        /// Address to listen on (0.0.0.0 = reachable from phones on your network).
        #[arg(long, default_value = "0.0.0.0")]
        bind: std::net::IpAddr,
        /// First port for LocalSend targets (one per TV, counting up).
        #[arg(long, default_value_t = 53317)]
        localsend_port: u16,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

fn category_id(name: &str) -> Option<String> {
    match name.to_ascii_lowercase().as_str() {
        "all" => None,
        "my-photos" | "mine" | "photos" => Some(category::MY_PHOTOS.into()),
        "favorites" | "favourites" | "fav" => Some(category::FAVORITES.into()),
        "store" | "art-store" => Some(category::STORE.into()),
        _ => Some(name.to_string()),
    }
}

struct App {
    cfg: Config,
    cfg_path: PathBuf,
    state_dir: PathBuf,
    json: bool,
    wake: bool,
}

impl App {
    fn save(&self) -> Result<()> {
        self.cfg.save(&self.cfg_path).with_context(|| format!("saving {}", self.cfg_path.display()))
    }

    fn select(&self, keys: &[String], all: bool) -> Result<Vec<TvConfig>> {
        if self.cfg.tvs.is_empty() {
            bail!("no TVs configured yet; run `frametv discover` and then `frametv pair <ip>`");
        }
        if all {
            return Ok(self.cfg.tvs.clone());
        }
        if !keys.is_empty() {
            return keys
                .iter()
                .map(|k| self.cfg.find(k).cloned().ok_or_else(|| anyhow!("no TV named `{k}` (see `frametv tvs`)")))
                .collect();
        }
        if let Some(d) = &self.cfg.default_tv
            && let Some(tv) = self.cfg.find(d)
        {
            return Ok(vec![tv.clone()]);
        }
        match self.cfg.tvs.as_slice() {
            [only] => Ok(vec![only.clone()]),
            _ => bail!(
                "several TVs configured; pick with --tv <name> or --all (or set one with `configure <tv> --default`)"
            ),
        }
    }

    fn select_one(&self, keys: &[String], all: bool) -> Result<TvConfig> {
        let mut tvs = self.select(keys, all)?;
        if tvs.len() > 1 {
            bail!("this command works on one TV at a time");
        }
        Ok(tvs.remove(0))
    }

    /// Connect to the art channel, waking the TV first if asked, and store any refreshed token.
    async fn connect(&mut self, tv: &TvConfig) -> Result<ArtClient> {
        if self.wake {
            wake_and_wait(tv).await?;
        }
        let endpoint = tv.endpoint(&self.cfg.client_name);
        let art =
            ArtClient::connect(&endpoint).await.with_context(|| format!("connecting to {} ({})", tv.name, tv.host))?;
        if let Some(token) = art.issued_token()
            && tv.token.as_deref() != Some(token)
            && let Some(stored) = self.cfg.find_mut(&tv.name)
        {
            stored.token = Some(token.to_string());
            self.save()?;
        }
        Ok(art)
    }

    fn state_file(&self, tv: &TvConfig) -> PathBuf {
        sync::state_path(&self.state_dir, &tv.name)
    }

    fn upload_settings(
        &self,
        tv: &TvConfig,
        fit: Option<Fit>,
        matte: Option<String>,
        portrait: Option<String>,
    ) -> Result<UploadSettings> {
        let fit = match fit {
            Some(f) => f,
            None => tv.fit.as_deref().map(str::parse).transpose().map_err(|e: String| anyhow!(e))?.unwrap_or_default(),
        };
        let (width, height) = tv.panel_size();
        Ok(UploadSettings {
            prep: PrepOptions { fit, width, height, ..Default::default() },
            matte: matte.or_else(|| tv.matte.clone()).unwrap_or_else(|| "auto".into()),
            portrait_matte: portrait.or_else(|| tv.portrait_matte.clone()).unwrap_or_else(|| "shadowbox_polar".into()),
        })
    }

    fn print_json(&self, v: &impl serde::Serialize) -> Result<()> {
        println!("{}", serde_json::to_string_pretty(v)?);
        Ok(())
    }
}

async fn wake_and_wait(tv: &TvConfig) -> Result<()> {
    if rest::device_info(&tv.host, Duration::from_secs(2)).await.is_ok_and(|i| i.power_state() == "on") {
        return Ok(());
    }
    let mac = tv
        .mac
        .as_deref()
        .ok_or_else(|| anyhow!("{} has no MAC address configured (`configure {} --mac ..`)", tv.name, tv.name))?;
    eprintln!("waking {}...", tv.name);
    for _ in 0..20 {
        wol::wake(mac, Some(&tv.host))?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        if rest::device_info(&tv.host, Duration::from_secs(1)).await.is_ok() {
            // Give the art app a moment after the network stack comes up.
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
    }
    bail!("{} did not wake up (is network standby / Power On with Mobile enabled?)", tv.name)
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn item_line(item: &ArtItem, current: Option<&str>, state: Option<&TvState>) -> String {
    let marker = if current == Some(item.content_id.as_str()) { "▶" } else { " " };
    let size = match (item.width, item.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => String::new(),
    };
    let source = state
        .and_then(|s| s.uploads.get(&item.content_id))
        .and_then(|r| r.source.file_name())
        .map(|n| format!("  ← {}", n.to_string_lossy()))
        .unwrap_or_default();
    format!(
        "{marker} {:<16} {:>10}  {:<19}  {}{source}",
        item.content_id,
        size,
        item.image_date.as_deref().unwrap_or(""),
        item.matte_id.as_deref().unwrap_or(""),
    )
}

fn header(app: &App, tvs: &[TvConfig], tv: &TvConfig) {
    if tvs.len() > 1 && !app.json {
        println!("── {} ({}) ──", tv.name, tv.host);
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "warn",
        1 => "info,samsung_frame=debug",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let cfg_path = cli.config.clone().unwrap_or_else(config::default_config_path);
    let cfg = Config::load(&cfg_path)?;
    let mut app = App { cfg, cfg_path, state_dir: config::default_state_dir(), json: cli.json, wake: cli.wake };
    let (keys, all) = (cli.tvs.clone(), cli.all);

    match cli.command {
        Command::Discover { scan, wait } => discover(&app, scan, wait).await,
        Command::Pair { host, name, port, default } => pair(&mut app, host, name, port, default).await,
        Command::Serve { port, bind, localsend_port } => {
            let opts = serve::ServeOptions { bind, port, localsend_port };
            serve::run(app.cfg, app.cfg_path, app.state_dir, opts).await
        }
        Command::Tvs => {
            if app.json {
                return app.print_json(&app.cfg.tvs);
            }
            if app.cfg.tvs.is_empty() {
                println!("No TVs configured. Run `frametv discover`, then `frametv pair <ip>`.");
            }
            for tv in &app.cfg.tvs {
                let default = app.cfg.default_tv.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&tv.name));
                println!(
                    "{} {:<16} {:<15} {:<16} {}{}",
                    if default { "*" } else { " " },
                    tv.name,
                    tv.host,
                    tv.model.as_deref().unwrap_or("?"),
                    if tv.token.is_some() { "paired" } else { "not paired" },
                    tv.folder.as_ref().map(|f| format!("  folder: {}", f.display())).unwrap_or_default(),
                );
            }
            println!("\nconfig: {}", app.cfg_path.display());
            Ok(())
        }
        Command::Configure { tv, folder, matte, portrait_matte, fit, host, mac, rename, default } => {
            let entry = app.cfg.find_mut(&tv).ok_or_else(|| anyhow!("no TV named `{tv}`"))?;
            if let Some(f) = folder {
                let abs = std::path::absolute(&f).unwrap_or(f);
                if !abs.is_dir() {
                    bail!("{} is not a folder", abs.display());
                }
                entry.folder = Some(abs);
            }
            if let Some(m) = matte {
                entry.matte = Some(m);
            }
            if let Some(m) = portrait_matte {
                entry.portrait_matte = Some(m);
            }
            if let Some(f) = fit {
                entry.fit = Some(f.to_string());
            }
            if let Some(h) = host {
                entry.host = h;
            }
            if let Some(m) = mac {
                wol::parse_mac(&m)?;
                entry.mac = Some(m);
            }
            let old_name = entry.name.clone();
            if let Some(n) = rename {
                entry.name = n;
                let (old, new) =
                    (sync::state_path(&app.state_dir, &old_name), sync::state_path(&app.state_dir, &entry.name));
                if old.exists() {
                    std::fs::rename(old, new)?;
                }
            }
            let name = entry.name.clone();
            if default || app.cfg.default_tv.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&old_name)) {
                app.cfg.default_tv = Some(name.clone());
            }
            app.save()?;
            println!("updated {name}");
            Ok(())
        }
        Command::Forget { tv } => {
            let removed = app.cfg.remove(&tv).ok_or_else(|| anyhow!("no TV named `{tv}`"))?;
            app.save()?;
            println!("forgot {} ({})", removed.name, removed.host);
            Ok(())
        }
        Command::Status => {
            let tvs = app.select(&keys, all)?;
            let mut out = Vec::new();
            for tv in &tvs {
                let s = status(&mut app, tv).await;
                if app.json {
                    out.push(s);
                }
            }
            if app.json { app.print_json(&out) } else { Ok(()) }
        }
        Command::List { category } => {
            let tvs = app.select(&keys, all)?;
            let mut out = serde_json::Map::new();
            for tv in &tvs {
                header(&app, &tvs, tv);
                let art = app.connect(tv).await?;
                let items = art.content_list(category_id(&category).as_deref()).await?;
                let current = art.current_artwork().await.ok().map(|c| c.content_id);
                if app.json {
                    out.insert(tv.name.clone(), serde_json::to_value(&items)?);
                    continue;
                }
                let state = TvState::load(&app.state_file(tv)).ok();
                for item in &items {
                    println!("{}", item_line(item, current.as_deref(), state.as_ref()));
                }
                println!("{} item(s)", items.len());
            }
            if app.json { app.print_json(&out) } else { Ok(()) }
        }
        Command::Current => {
            for tv in app.select(&keys, all)? {
                let art = app.connect(&tv).await?;
                let cur = art.current_artwork().await?;
                if app.json {
                    app.print_json(&cur)?;
                } else {
                    let state = TvState::load(&app.state_file(&tv)).ok();
                    println!("{}: {}", tv.name, item_line(&cur, None, state.as_ref()).trim());
                }
            }
            Ok(())
        }
        Command::Upload { paths, fit, matte, portrait_matte, show } => {
            let files = expand_paths(&paths)?;
            if files.is_empty() {
                bail!("no supported images found");
            }
            for tv in app.select(&keys, all)? {
                let settings = app.upload_settings(&tv, fit, matte.clone(), portrait_matte.clone())?;
                let art = app.connect(&tv).await?;
                let state_file = app.state_file(&tv);
                let mut state = TvState::load(&state_file)?;
                let mut last = None;
                for (i, path) in files.iter().enumerate() {
                    eprint!("[{}/{}] {} → {} ... ", i + 1, files.len(), path.display(), tv.name);
                    match sync::upload_file(&art, path, &settings).await {
                        Ok(r) => {
                            state.record(&r.content_id, &sync::hash_file(path)?, path);
                            state.save(&state_file)?;
                            eprintln!(
                                "{} ({}x{}, {} KB, matte {})",
                                r.content_id,
                                r.width,
                                r.height,
                                r.bytes / 1024,
                                r.matte
                            );
                            if app.json {
                                println!("{}", json!({"tv": tv.name, "path": path, "content_id": r.content_id}));
                            }
                            last = Some(r.content_id);
                        }
                        Err(e) => eprintln!("failed: {e}"),
                    }
                }
                if show && let Some(id) = last {
                    art.select_image(&id, None, true).await?;
                }
            }
            Ok(())
        }
        Command::Sync { folder, delete, dry_run, fit, matte, portrait_matte } => {
            for tv in app.select(&keys, all)? {
                let folder = folder.clone().or_else(|| tv.folder.clone()).ok_or_else(|| {
                    anyhow!(
                        "no folder given and none configured for {} (`configure {} --folder <dir>`)",
                        tv.name,
                        tv.name
                    )
                })?;
                run_sync(&mut app, &tv, &folder, delete, dry_run, fit, matte.clone(), portrait_matte.clone()).await?;
            }
            Ok(())
        }
        Command::Delete { ids, all_mine, yes } => {
            for tv in app.select(&keys, all)? {
                let state_file = app.state_file(&tv);
                let mut state = TvState::load(&state_file)?;
                let ids: Vec<String> = if all_mine { state.uploads.keys().cloned().collect() } else { ids.clone() };
                if ids.is_empty() {
                    println!("{}: nothing to delete", tv.name);
                    continue;
                }
                if !yes && !confirm(&format!("Delete {} image(s) from {}?", ids.len(), tv.name))? {
                    continue;
                }
                let art = app.connect(&tv).await?;
                art.delete(&ids).await?;
                for id in &ids {
                    state.uploads.remove(id);
                }
                state.save(&state_file)?;
                println!("{}: deleted {}", tv.name, ids.len());
            }
            Ok(())
        }
        Command::Show { id, quiet } => {
            for tv in app.select(&keys, all)? {
                app.connect(&tv).await?.select_image(&id, None, !quiet).await?;
            }
            Ok(())
        }
        Command::Artmode { state } => {
            for tv in app.select(&keys, all)? {
                let art = app.connect(&tv).await?;
                if let Some(s) = state {
                    art.set_artmode(matches!(s, OnOff::On)).await?;
                } else {
                    let on = art.artmode().await?;
                    if app.json {
                        println!("{}", json!({"tv": tv.name, "artmode": on}));
                    } else {
                        println!("{}: art mode {}", tv.name, if on { "on" } else { "off" });
                    }
                }
            }
            Ok(())
        }
        Command::Slideshow { interval, sequential, category } => {
            let cat = category_id(&category).unwrap_or_else(|| category::MY_PHOTOS.into());
            for tv in app.select(&keys, all)? {
                let art = app.connect(&tv).await?;
                match interval.as_deref() {
                    None => {
                        let s = art.slideshow_status().await?;
                        if app.json {
                            app.print_json(&s)?;
                        } else {
                            match s.minutes {
                                None => println!("{}: slideshow off", tv.name),
                                Some(m) => println!(
                                    "{}: {} every {}, from {}",
                                    tv.name,
                                    if s.shuffle { "shuffle" } else { "in order" },
                                    human_minutes(m),
                                    s.category_id.as_deref().unwrap_or("?")
                                ),
                            }
                        }
                    }
                    Some(v) => {
                        let minutes = if v.eq_ignore_ascii_case("off") { None } else { Some(parse_interval(v)?) };
                        art.set_slideshow(minutes, !sequential, &cat).await?;
                    }
                }
            }
            Ok(())
        }
        Command::Mattes => {
            let tv = app.select_one(&keys, all)?;
            let m = app.connect(&tv).await?.matte_list().await?;
            if app.json {
                return app.print_json(&m);
            }
            println!("types:  {}", m.types.join(", "));
            if !m.colors.is_empty() {
                println!("colors: {}", m.colors.join(", "));
                println!("\nuse as <type>_<color>, e.g. shadowbox_polar, or `none`");
            }
            Ok(())
        }
        Command::Matte { id, matte, portrait } => {
            for tv in app.select(&keys, all)? {
                app.connect(&tv).await?.change_matte(&id, &matte, portrait.as_deref()).await?;
            }
            Ok(())
        }
        Command::Filters => {
            let tv = app.select_one(&keys, all)?;
            let f = app.connect(&tv).await?.photo_filters().await?;
            if app.json {
                app.print_json(&f)
            } else {
                println!("{}", f.join(", "));
                Ok(())
            }
        }
        Command::Filter { id, filter } => {
            for tv in app.select(&keys, all)? {
                app.connect(&tv).await?.set_photo_filter(&id, &filter).await?;
            }
            Ok(())
        }
        Command::Settings => {
            for tv in app.select(&keys, all)? {
                let settings = app.connect(&tv).await?.artmode_settings().await?;
                if app.json {
                    app.print_json(&settings)?;
                    continue;
                }
                println!("{}:", tv.name);
                for s in settings {
                    let range = match (&s.min, &s.max) {
                        (Some(a), Some(b)) => format!("  ({a}..{b})"),
                        _ => s.valid_values.as_ref().map(|v| format!("  {v}")).unwrap_or_default(),
                    };
                    println!("  {:<28} {}{range}", s.item, s.value.as_deref().unwrap_or("?"));
                }
            }
            Ok(())
        }
        Command::Set { item, value } => {
            for tv in app.select(&keys, all)? {
                app.connect(&tv).await?.set_setting(&item, &value).await?;
            }
            Ok(())
        }
        Command::Favorite { id, remove } => {
            for tv in app.select(&keys, all)? {
                app.connect(&tv).await?.set_favorite(&id, !remove).await?;
            }
            Ok(())
        }
        Command::Thumbs { ids, out } => {
            let tv = app.select_one(&keys, all)?;
            let art = app.connect(&tv).await?;
            let ids = if ids.is_empty() {
                art.content_list(Some(category::MY_PHOTOS)).await?.into_iter().map(|i| i.content_id).collect()
            } else {
                ids
            };
            std::fs::create_dir_all(&out)?;
            let thumbs = art.thumbnails(&ids).await?;
            for t in &thumbs {
                let path = out.join(format!("{}.{}", t.content_id, t.file_type));
                std::fs::write(&path, &t.data)?;
            }
            println!("saved {} thumbnail(s) to {}", thumbs.len(), out.display());
            Ok(())
        }
        Command::Wake => {
            for tv in app.select(&keys, all)? {
                wake_and_wait(&tv).await?;
                println!("{} is up", tv.name);
            }
            Ok(())
        }
        Command::Key { key, hold } => {
            let key = if key.to_ascii_uppercase().starts_with("KEY_") {
                key.to_ascii_uppercase()
            } else {
                format!("KEY_{}", key.to_ascii_uppercase())
            };
            for tv in app.select(&keys, all)? {
                let mut rc = remote::RemoteClient::connect(&tv.endpoint(&app.cfg.client_name)).await?;
                match hold {
                    Some(s) => rc.hold(&key, Duration::from_secs_f32(s)).await?,
                    None => rc.click(&key).await?,
                }
                rc.close().await;
            }
            Ok(())
        }
        Command::Watch => {
            let tv = app.select_one(&keys, all)?;
            let art = app.connect(&tv).await?;
            let mut rx = art.subscribe();
            eprintln!("watching {} (Ctrl-C to stop)", tv.name);
            loop {
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Ok(m) => println!("{} {}", m.event, m.data),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => bail!("connection closed"),
                    },
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                }
            }
        }
        Command::Raw { request } => {
            let v: Value = serde_json::from_str(&request).context("request must be JSON")?;
            let tv = app.select_one(&keys, all)?;
            let reply = app.connect(&tv).await?.request(v).await?;
            app.print_json(&reply)
        }
    }
}

fn human_minutes(m: u32) -> String {
    match m {
        m if m % 1440 == 0 => format!("{} day(s)", m / 1440),
        m if m % 60 == 0 => format!("{} hour(s)", m / 60),
        m => format!("{m} min"),
    }
}

/// `15`, `15m`, `2h`, `1d`.
fn parse_interval(s: &str) -> Result<u32> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = match s.chars().last() {
        Some('m') => (&s[..s.len() - 1], 1),
        Some('h') => (&s[..s.len() - 1], 60),
        Some('d') => (&s[..s.len() - 1], 1440),
        _ => (s.as_str(), 1),
    };
    let n: u32 = num.parse().map_err(|_| anyhow!("invalid interval `{s}` (use e.g. 15, 3h, 1d or off)"))?;
    Ok(n * mult)
}

fn expand_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            out.extend(sync::scan_folder(p)?.into_iter().map(|l| l.path));
        } else if p.is_file() {
            out.push(p.clone());
        } else {
            bail!("{} not found", p.display());
        }
    }
    Ok(out)
}

async fn discover(app: &App, scan: bool, wait: u64) -> Result<()> {
    eprintln!("searching...");
    let mut found = discovery::ssdp(Duration::from_secs(wait)).await?;
    if scan || found.is_empty() {
        if !scan {
            eprintln!("nothing answered SSDP; scanning nearby subnets...");
        }
        let seen: HashSet<_> = found.iter().map(|d| d.ip).collect();
        found.extend(discovery::scan_subnet().await?.into_iter().filter(|d| !seen.contains(&d.ip)));
    }
    found.sort_by_key(|d| d.ip);
    if app.json {
        let v: Vec<Value> = found.iter().map(|d| json!({"ip": d.ip, "info": d.info})).collect();
        return app.print_json(&v);
    }
    if found.is_empty() {
        println!("No Samsung TVs found. Make sure the TV is on and on the same network.");
        return Ok(());
    }
    for d in &found {
        let ip = d.ip.to_string();
        let known = app.cfg.find(&ip).map(|t| format!("  (configured as {})", t.name)).unwrap_or_default();
        println!(
            "{:<15} {:<28} {:<18} {:<8} {}{known}",
            ip,
            d.info.display_name(),
            d.info.device.model_name.as_deref().unwrap_or("?"),
            d.info.power_state(),
            if d.info.is_frame() { "Frame" } else { "not a Frame" },
        );
    }
    if found.iter().any(|d| d.info.is_frame() && app.cfg.find(&d.ip.to_string()).is_none()) {
        println!("\nPair with: frametv pair <ip> --name <name>");
    }
    Ok(())
}

async fn pair(app: &mut App, host: String, name: Option<String>, port: u16, make_default: bool) -> Result<()> {
    eprintln!("Connecting to {host}; if a prompt appears on the TV, choose Allow...");
    let p = pairing::pair(&app.cfg, &host, name.as_deref(), port, Duration::from_secs(60)).await?;
    if !p.is_frame {
        eprintln!("warning: {} does not report Frame TV support; art commands may not work", p.tv.name);
    }
    let first = app.cfg.tvs.is_empty();
    let tv_name = p.tv.name.clone();
    app.cfg.upsert(p.tv);
    if make_default || first {
        app.cfg.default_tv = Some(tv_name.clone());
    }
    app.save()?;
    println!(
        "Paired with {tv_name} (art API {}{}). Saved to {}",
        p.api_version.as_deref().unwrap_or("?"),
        p.photo_count.map(|c| format!(", {c} photos in My Photos")).unwrap_or_default(),
        app.cfg_path.display()
    );
    Ok(())
}

async fn status(app: &mut App, tv: &TvConfig) -> Value {
    let mut v = json!({"name": tv.name, "host": tv.host});
    let info = rest::device_info(&tv.host, Duration::from_secs(3)).await;
    if !app.json {
        println!("{} ({})", tv.name, tv.host);
    }
    match &info {
        Ok(i) => {
            v["power"] = json!(i.power_state());
            v["model"] = json!(i.device.model_name);
            if !app.json {
                println!(
                    "  model       {} (20{:02}), {}",
                    i.device.model_name.as_deref().unwrap_or("?"),
                    i.model_year(),
                    i.device.resolution.as_deref().unwrap_or("?")
                );
                println!("  power       {}", i.power_state());
            }
        }
        Err(e) => {
            v["power"] = json!("unreachable");
            if !app.json {
                println!("  unreachable: {e}");
            }
            return v;
        }
    }
    let art = match app.connect(tv).await {
        Ok(a) => a,
        Err(e) => {
            if !app.json {
                println!("  art channel {e:#}");
            }
            v["error"] = json!(format!("{e:#}"));
            return v;
        }
    };
    if let Ok(api) = art.api_version().await {
        v["api_version"] = json!(api);
    }
    if let Ok(on) = art.artmode().await {
        v["artmode"] = json!(on);
        if !app.json {
            println!("  art mode    {}", if on { "on" } else { "off" });
        }
    }
    if let Ok(cur) = art.current_artwork().await {
        if !app.json {
            println!("  showing     {} ({})", cur.content_id, cur.matte_id.as_deref().unwrap_or("no matte"));
        }
        v["current"] = json!(cur.content_id);
    }
    if let Ok(list) = art.content_list(Some(category::MY_PHOTOS)).await {
        let state = TvState::load(&app.state_file(tv)).unwrap_or_default();
        let mine = list.iter().filter(|i| state.uploads.contains_key(&i.content_id)).count();
        if !app.json {
            println!("  my photos   {} ({} uploaded by frametv)", list.len(), mine);
        }
        v["my_photos"] = json!(list.len());
    }
    if let Ok(s) = art.slideshow_status().await {
        if !app.json {
            match s.minutes {
                None => println!("  slideshow   off"),
                Some(m) => println!(
                    "  slideshow   {} every {}",
                    if s.shuffle { "shuffle" } else { "in order" },
                    human_minutes(m)
                ),
            }
        }
        v["slideshow_minutes"] = json!(s.minutes);
    }
    if let Some(f) = &tv.folder
        && !app.json
    {
        println!("  sync folder {}", f.display());
    }
    v
}

#[allow(clippy::too_many_arguments)]
async fn run_sync(
    app: &mut App,
    tv: &TvConfig,
    folder: &Path,
    delete: bool,
    dry_run: bool,
    fit: Option<Fit>,
    matte: Option<String>,
    portrait: Option<String>,
) -> Result<()> {
    let settings = app.upload_settings(tv, fit, matte, portrait)?;
    eprintln!("scanning {}...", folder.display());
    let folder_owned = folder.to_path_buf();
    let local = tokio::task::spawn_blocking(move || sync::scan_folder(&folder_owned)).await??;

    let art = app.connect(tv).await?;
    let on_tv: HashSet<String> =
        art.content_list(Some(category::MY_PHOTOS)).await?.into_iter().map(|i| i.content_id).collect();
    let state_file = app.state_file(tv);
    let mut state = TvState::load(&state_file)?;
    let plan = sync::plan(&local, &state, &on_tv, delete);

    println!(
        "{}: {} local image(s): {} to upload, {} to delete, {} already there",
        tv.name,
        local.len(),
        plan.upload.len(),
        plan.delete.len(),
        plan.unchanged
    );
    if dry_run {
        for l in &plan.upload {
            println!("  + {}", l.path.display());
        }
        for (id, src) in &plan.delete {
            println!("  - {id} ({})", src.display());
        }
        return Ok(());
    }
    if plan.is_empty() {
        return Ok(());
    }
    let outcome = sync::execute(&art, &mut state, &state_file, plan, &settings, |ev| match ev {
        SyncEvent::Uploading { index, total, path } => {
            eprint!("  [{}/{}] {} ... ", index + 1, total, path.file_name().unwrap_or_default().to_string_lossy());
        }
        SyncEvent::Uploaded { result, .. } => {
            eprintln!(
                "{} ({}x{}{})",
                result.content_id,
                result.width,
                result.height,
                if result.cropped { ", cropped" } else { "" }
            );
        }
        SyncEvent::UploadFailed { error, .. } => eprintln!("failed: {error}"),
        SyncEvent::Deleted { content_id, source } => eprintln!("  deleted {content_id} ({})", source.display()),
    })
    .await?;
    println!(
        "{}: uploaded {}, deleted {}, failed {}",
        tv.name,
        outcome.uploaded.len(),
        outcome.deleted,
        outcome.failed
    );
    if outcome.failed > 0 {
        bail!("{} upload(s) failed", outcome.failed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals() {
        assert_eq!(parse_interval("15").unwrap(), 15);
        assert_eq!(parse_interval("3h").unwrap(), 180);
        assert_eq!(parse_interval("1d").unwrap(), 1440);
        assert!(parse_interval("soon").is_err());
        assert_eq!(human_minutes(120), "2 hour(s)");
    }

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
