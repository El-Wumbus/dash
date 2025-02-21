use log::{info, warn, error};
use rinja::Template as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::str::FromStr;
use tiny_http::{Header, Method, Request, Response};
use std::time::Duration;
use signal_hook::consts::SIGHUP;
use std::sync::{Arc, atomic::{Ordering, AtomicBool}};
use std::path::Path;

#[allow(dead_code)]
mod uri;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default = "Config::default_bind")]
    bind: String,
    #[serde(default)]
    apps: Vec<Entry>,
}

impl Config {
    fn default_bind() -> String {
        String::from("127.0.0.1:8333")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
            apps: Vec::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    url: String,
    desc: Option<String>,
    icon: Option<Icon>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Icon {
    Remote { url: String },
    Local { path: String },
}

fn load_config(config_file: &Path) -> eyre::Result<Config> {
    let config = if !config_file.exists() {
        let c = Config::default();
        let t = toml::ser::to_string_pretty(&c)?;
        let mut f = File::create(&config_file)?;
        f.write_all(t.as_bytes())?;
        c
    } else {
        let contents = fs::read_to_string(&config_file)?;
        toml::de::from_str(&contents)?
    };
    Ok(config)
}

fn main() -> eyre::Result<()> {
    use log::LevelFilter;
    env_logger::Builder::new()
        .filter(None, LevelFilter::Debug)
        .init();

    let reload_state = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGHUP, reload_state.clone())?;

    let config_dir = dirs::config_dir().expect("config directory").join("dash");
    let config_file = config_dir.join("config.toml");
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)?;
    }

    let mut config = load_config(&config_file)?;
    let bind = match std::net::SocketAddr::from_str(&config.bind) {
        Ok(b) => b,
        Err(e) => {
            error!("Invalid bind address: {e}");
            std::process::exit(1);
        }
    };

    let mut local_icons = Vec::new();
    for app in config.apps.iter() {
        if let Some(Icon::Local { path }) = &app.icon {
            local_icons.push(path.clone());
        }
    }

    let server = tiny_http::Server::http(bind)
        .map_err(|e| eyre::eyre!("Failed to run server: {e}"))?;

    loop {
        // blocks until the next request is received
        let Some(request) = (match server.recv_timeout(Duration::from_millis(420)) {
            Ok(rq) => rq,
            Err(e) => {
                error!("{}", e);
                break;
            }
        }) else {
            if reload_state.swap(false, Ordering::Relaxed) {
                info!("Reloading state...");
                match load_config(&config_file) {
                    Ok(c) => {
                        config = c;
                        info!("Reloaded config from {config_file:?}");
                    },
                    Err(e) => {
                        error!("Failed to reload config file {config_file:?}: {e}");
                        warn!("Using old configuration");
                    }
                }
                
            }
            continue;
        };

        let method = request.method();
        let Some(host) = request
            .headers()
            .iter()
            .find(|x| x.field.as_str().as_str().eq_ignore_ascii_case("host"))
            .map(|x| x.value.as_str())
        else {
            error!("Request is missing \"Host\" header!");
            respond_or_log(
                request,
                Response::from_string("Missing \"Host\" header").with_status_code(400),
            );
            continue;
        };
        let (host, _port) = match host.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host, None),
        };

        let Ok(url) = uri::Uri::new(request.url()) else {
            respond_or_log(request, Response::empty(400));
            continue;
        };
        let path = url.path.unwrap();
        match (path, method) {
            ("/", Method::Get) => {
                let apps = config
                    .apps
                    .iter()
                    .map(|app| match uri::Uri::new(&app.url) {
                        Ok(url) if url.host.is_none_or(str::is_empty) => {
                            let mut url = uri::UriOwned::from(url);
                            url.host = Some(host.to_string());
                            let mut e = app.clone();
                            e.url = url.to_string();
                            return e;
                        }
                        _ => app.clone(),
                    })
                    .collect::<Vec<_>>();
                let Ok(html) = Template { apps: &apps }.render() else {
                    error!("Failed to render template!");
                    respond_or_log(
                        request,
                        Response::from_string("Failed to render response")
                            .with_status_code(500),
                    );
                    continue;
                };
                respond_or_log(
                    request,
                    Response::from_string(html).with_header(
                        Header::from_bytes(b"Content-Type", b"text/html").unwrap(),
                    ),
                );
            }
            _ if path.starts_with("/icon/")
                && local_icons.iter().any(|x| x == &path["/icon/".len()..]) =>
            {
                let path = &path["/icon/".len()..];
                let Ok(mut f) = File::open(&path) else {
                    // TODO: don't assume not-found
                    respond_or_log(request, Response::empty(404));
                    continue;
                };
                let mut contents = vec![];
                if f.read_to_end(&mut contents).is_err() {
                    respond_or_log(request, Response::empty(500));
                    continue;
                };

                respond_or_log(request, Response::from_data(contents));
            }
            _ => {
                respond_or_log(request, Response::empty(404));
            }
        }
    }

    Ok(())
}

#[derive(Debug, rinja::Template)]
#[template(ext = "html", path = "template.html")]
struct Template<'a> {
    apps: &'a [Entry],
}

fn respond_or_log<R: Read>(request: Request, response: Response<R>) {
    if let Err(e) = request.respond(response) {
        error!("Failed to respond to request: {e}");
    }
}
