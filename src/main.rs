use log::error;
use rinja::Template as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use tiny_http::{Header, Method, Request, Response};
#[allow(dead_code)]
mod uri;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    apps: Vec<Entry>,
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

fn main() -> eyre::Result<()> {
    use log::LevelFilter;
    env_logger::Builder::new()
        .filter(None, LevelFilter::Debug)
        .init();

    let config_dir = dirs::config_dir().expect("config directory").join("dash");
    let config_file = config_dir.join("config.toml");
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)?;
    }

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

    let mut local_icons = Vec::new();
    for app in config.apps.iter() {
        if let Some(Icon::Local { path }) = &app.icon {
            local_icons.push(path.clone());
        }
    }

    let server = tiny_http::Server::http("127.0.0.1:3000").unwrap();
    loop {
        // blocks until the next request is received
        let request = match server.recv() {
            Ok(rq) => rq,
            Err(e) => {
                error!("{}", e);
                break;
            }
        };

        let method = request.method();
        let host = request
            .headers()
            .iter()
            .find(|x| x.field.as_str().as_str().eq_ignore_ascii_case("host"))
            .expect("Expected \"host\" request header!");

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
                        Ok(url) if url.host.is_none() => {
                            let mut url = uri::UriOwned::from(url);
                            url.host = Some(host.to_string());
                            let mut e = app.clone();
                            e.url = url.to_string();
                            return e;
                        }
                        _ => app.clone(),
                    })
                    .collect::<Vec<_>>();
                let html = Template { apps: &apps }.render()?;
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
                let Ok(mut f) = File::open(path) else {
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
