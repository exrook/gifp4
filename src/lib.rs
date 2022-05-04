use sled::{Batch, Db, IVec};
use std::str::Utf8Error;
use http_body::{combinators::BoxBody, Full};
use hyper::service::{make_service_fn, service_fn};
use hyper::Uri;
use hyper::{
    body::{Buf, HttpBody},
    service::Service,
    header::CONTENT_TYPE,
    Body, Method, Request, Response, Server, StatusCode,
};
use std::net::SocketAddr;
use std::collections::HashMap;
use warp::Filter;
use warp::Reply;
use std::io::Cursor;



type ID = [u8; 6];

fn cdn_suffix(url: &str) -> Option<String> {
    let s: String = dbg!(ammonia::Url::parse(url)).ok()?.into();
    s.split_once("discordapp.com/attachments/")
        .or(url.split_once("discordapp.net/attachments/"))
        .map(|(_, s)| s.to_owned())
}

fn parse_id(id: &str) -> Option<ID> {
    let mut id_bytes = [0; 6];
    base64::decode_config_slice(id, base64::URL_SAFE_NO_PAD, &mut id_bytes)
        .ok()
        .filter(|len| *len == 6)
        .map(|_| id_bytes)
}

fn format_id(id: &ID) -> String {
    base64::encode_config(id, base64::URL_SAFE_NO_PAD)
}

macro_rules! append_key {
    ($key:ident, $suffix:expr) => {
        [
            $key[0], $key[1], $key[2], $key[3], $key[4], $key[5], $suffix,
        ]
    };
}

fn insert(db: &Db, gif: &str, mp4: &str) -> sled::Result<ID> {
    let mut count = 0;
    let id = loop {
        let id: ID = rand::random();
        if let None = db.insert(&id, &[])? {
            break id;
        }
        count += 1;
        if count > 10 {
            return Err(sled::Error::Unsupported("out of IDs".to_owned()));
        }
    };
    let gif_key = append_key!(id, 1);
    let mp4_key = append_key!(id, 2);
    let mut batch = Batch::default();
    batch.insert(&gif_key, gif);
    batch.insert(&mp4_key, mp4);
    db.apply_batch(batch)?;
    Ok(id)
}

/// IVec we know to be a UTF-8 string
struct IString(IVec);

impl IString {
    fn new(vec: IVec) -> Result<Self, Utf8Error> {
        let _ = std::str::from_utf8(&*vec)?;
        Ok(Self(vec))
    }
}

impl std::ops::Deref for IString {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        // SAFETY: we validate this constraint in the constructor
        unsafe { std::str::from_utf8_unchecked(&*self.0) }
    }
}

fn lookup(db: &Db, id: &ID) -> sled::Result<Option<(IString, IString)>> {
    let gif_key = append_key!(id, 1);
    let mp4_key = append_key!(id, 2);
    let gif = match db.get(gif_key)?.and_then(|v| IString::new(v).ok()) {
        None => return Ok(None),
        Some(x) => x,
    };
    let mp4 = match db.get(mp4_key)?.and_then(|v| IString::new(v).ok()) {
        None => return Ok(None),
        Some(x) => x,
    };
    Ok(Some((gif, mp4)))
}

type MyBody = BoxBody<Box<dyn Buf + 'static + Send + Sync>, hyper::Error>;

fn set_ct(resp: Result<Response<MyBody>, hyper::Error>, ctype: &str) -> Result<Response<MyBody>, hyper::Error> {
    resp.map(|mut resp| {
        match ctype {
            "html" => {
                resp.headers_mut().insert(CONTENT_TYPE, "text/html; charset=utf-8".parse().unwrap());
            }
            "css" => {
                resp.headers_mut().insert(CONTENT_TYPE, "text/css; charset=utf-8".parse().unwrap());
            }
            _ => {
                eprintln!("Skill issue");
            }
        }

        resp
    })
}

async fn serve(db: &Db, req: Request<Body>) -> Result<Response<MyBody>, hyper::Error> {
    let id = req
        .uri()
        .path()
        .strip_prefix("/")
        .map(|s| s.split_once(".").map(|(p, _)| p).unwrap_or(s))
        .and_then(parse_id);
    match (req.method(), req.uri().path(), id) {
        (&Method::GET, "/", None) => set_ct(send_home(), "html"),
        (&Method::GET, "/1.css", None) => set_ct(send_css(1), "css"),
        (&Method::GET, "/2.css", None) => set_ct(send_css(2), "css"),
        (&Method::POST, "/submit", None) => set_ct(Ok(submit_request(db, req).await.unwrap()), "html"),
        (&Method::GET, _, Some(id)) => set_ct(Ok(generate_response(db, &id).await.unwrap()), "html"),
        _ => not_found(),
    }
}

async fn submit_request(db: &Db, req: Request<Body>) -> Result<Response<MyBody>, hyper::Error> {
    let route = warp::body::content_length_limit(1024)
        .and(warp::body::form())
        .map(|form: HashMap<String, String>| {
            if let Some((gif, mp4)) = form.get("preview").and_then(|s| cdn_suffix(s)).and_then(|gif| {
                form.get("content")
                    .and_then(|s| cdn_suffix(s))
                    .map(|mp4| (gif, mp4))
            }) {
                match insert(db, &gif, &mp4) {
                    Ok(id) => {
                        let uri = Uri::builder()
                            .path_and_query(format!("{}", format_id(&id)))
                            .build()
                            .unwrap();
                        warp::redirect::see_other(uri).into_response()
                    }
                    Err(e) => {
                        eprintln!("Error inserting: {:?}", e);
                        warp::reply::with_status(
                            warp::reply::reply(),
                            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                        )
                        .into_response()
                    }
                }
            } else {
                warp::reply::with_status(warp::reply::reply(), warp::http::StatusCode::BAD_REQUEST)
                    .into_response()
            }
        });
    let (parts, body) = warp::service(route).call(req).await.unwrap().into_parts();
    Ok(Response::from_parts(
        parts,
        BoxBody::new(body.map_data(|d| Box::new(d) as Box<dyn Buf + 'static + Send + Sync>)),
    ))
}

static P1: &'static str = &r#"<!DOCTYPE html><html><head><meta property="og:image" content="https://cdn.discordapp.com/attachments/"#;
static P2: &'static str = &r#""><meta property="og:image:type" content="image/gif"><meta property="og:image:height" content="202"><meta property="og:image:width" content="250"><meta property="og:type" content="video.other"><meta property="og:video:url" content="https://cdn.discordapp.com/attachments/"#;
static P3: &'static str = &r#""><meta property="og:video:height" content="202"><meta property="og:video:width" content="250"></head><body><h1><a href="submit">Submit a link</a></h1></body></html>"#;

fn make_page_response(gif: IString, mp4: IString) -> impl Buf + 'static + Send + Sync {
    P1.as_bytes()
        .chain(Cursor::new(gif.0))
        .chain(P2.as_bytes())
        .chain(Cursor::new(mp4.0))
        .chain(P3.as_bytes())
}

async fn generate_response(db: &Db, id: &ID) -> Result<Response<MyBody>, ()> {
    let (gif, mp4) = lookup(db, id).ok().flatten().ok_or(())?;
    Ok(Response::new(MyBody::new(
        Full::new(Box::new(make_page_response(gif, mp4)) as Box<_>).map_err(|_| todo!()),
    )))
}

static HOME_PAGE: &'static str = include_str!("html/index.html");
static CSS_1: &'static str = include_str!("css/pure-min.css");
static CSS_2: &'static str = include_str!("css/grid-responsive-min.css");
static NOT_FOUND: &'static str = include_str!("html/not_found.html");

fn send_home() -> Result<Response<MyBody>, hyper::Error> {
    Ok(Response::new(BoxBody::new(
        Full::new(Box::new(HOME_PAGE.as_bytes()) as _).map_err(|_| todo!()),
    )))
}

fn send_css(sel: u8) -> Result<Response<MyBody>, hyper::Error> {
    match sel {
        1 => {
            Ok(Response::new(BoxBody::new(
                Full::new(Box::new(CSS_1.as_bytes()) as _).map_err(|_| todo!()),
            )))
        }
        2 => {
            Ok(Response::new(BoxBody::new(
                Full::new(Box::new(CSS_2.as_bytes()) as _).map_err(|_| todo!()),
            )))
        }
        _ => {
            eprintln!("Skill issue");
            Ok(Response::new(BoxBody::new(
                Full::new(Box::new(NOT_FOUND.as_bytes()) as _).map_err(|_| todo!()),
            )))
        }
    }
}

fn not_found() -> Result<Response<MyBody>, hyper::Error> {
    let mut resp = Response::new(BoxBody::new(
        Full::new(Box::new(NOT_FOUND.as_bytes()) as _).map_err(|_| todo!()),
    ));
    *resp.status_mut() = StatusCode::NOT_FOUND;
    Ok(resp)
}

pub fn database() -> sled::Result<Db> {
    sled::Config::new().path("gifp4_db").open()
}

pub async fn start_server(db: &'static Db, addr: SocketAddr) -> Result<(), hyper::Error> {
    let make_svc =
        make_service_fn(|_conn| async { Ok::<_, hyper::Error>(service_fn(|r| serve(db, r))) });
    let server = Server::bind(&addr).serve(make_svc);

    server.await
}
