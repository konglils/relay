use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let port_arg = match args.next() {
        Some(value) => value,
        None => return usage("missing port argument"),
    };
    let root_arg = match args.next() {
        Some(value) => value,
        None => return usage("missing root path argument"),
    };
    if args.next().is_some() {
        return usage("too many arguments");
    }

    let port: u16 = match port_arg.parse() {
        Ok(value) => value,
        Err(_) => return usage("invalid port"),
    };
    let root = PathBuf::from(root_arg);
    let metadata = match std::fs::metadata(&root) {
        Ok(value) => value,
        Err(_) => return usage("root path does not exist or is not accessible"),
    };
    if !metadata.is_dir() {
        return usage("root path is not a directory");
    }

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let app = Router::new()
        .fallback_service(ServeDir::new(root.clone()).append_index_html_on_directories(true));

    println!("Serving {} on http://{}", root.display(), addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn usage(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Error: {message}");
    eprintln!("Usage: file-share <port> <root_path>");
    Err("invalid arguments".into())
}
