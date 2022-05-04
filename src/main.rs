use std::net::SocketAddr;
use sled::Db;
use gifp4::{database, start_server};

#[tokio::main]
async fn main() {
    println!("Discord stacked meme generator running on port {}", 4312);
    let addr = SocketAddr::from(([127, 0, 0, 1], 4312));
    let db: &Db = Box::leak(Box::new(database().expect("Unable to open DB")));
    if let Err(e) = start_server(db, addr).await {
        eprintln!("Server error: {}", e);
    }
}
