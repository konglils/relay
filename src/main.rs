use std::env;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Request, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use tower::ServiceExt;
use tower_http::services::ServeFile;

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
    let root = match root.canonicalize() {
        Ok(value) => value,
        Err(_) => return usage("failed to canonicalize root path"),
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let state = AppState { root };
    let app = Router::new()
        .route("/", get(handle_root))
        .route("/{*path}", get(handle_path))
        .with_state(state);

    println!("Serving on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn usage(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Error: {message}");
    eprintln!("Usage: file-share <port> <root_path>");
    Err("invalid arguments".into())
}

#[derive(Clone)]
struct AppState {
    root: PathBuf,
}

async fn handle_root(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response, Response> {
    handle_path_impl(state, "".to_string(), req).await
}

async fn handle_path(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    req: Request<Body>,
) -> Result<Response, Response> {
    handle_path_impl(state, path, req).await
}

async fn handle_path_impl(
    state: AppState,
    path: String,
    req: Request<Body>,
) -> Result<Response, Response> {
    let relative = sanitize_path(&path).ok_or_else(not_found)?;
    let full_path = state.root.join(&relative);

    let metadata = match tokio::fs::metadata(&full_path).await {
        Ok(value) => value,
        Err(_) => return Err(not_found()),
    };

    if metadata.is_dir() {
        let html = match render_directory_listing(&state.root, &relative).await {
            Ok(value) => value,
            Err(_) => return Err(server_error()),
        };
        Ok(Html(html).into_response())
    } else {
        let service = ServeFile::new(full_path);
        match service.oneshot(req).await {
            Ok(response) => Ok(response.into_response()),
            Err(_) => Err(server_error()),
        }
    }
}

fn sanitize_path(path: &str) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(result)
}

async fn render_directory_listing(root: &Path, relative: &Path) -> Result<String, std::io::Error> {
    let full_path = root.join(relative);
    let mut entries = tokio::fs::read_dir(&full_path).await?;
    let mut rows = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let name = entry.file_name().to_string_lossy().to_string();
        rows.push((name, file_type.is_dir()));
    }

    rows.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let base = relative.to_string_lossy();
    let base_prefix = if base.is_empty() {
        String::new()
    } else {
        format!("/{}", base)
    };

    let mut html = String::new();
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    html.push_str("<title>Index of ");
    html.push_str(&escape_html(&base_prefix));
    html.push_str("</title>");
    html.push_str("<style>body{font-family:system-ui,sans-serif;padding:24px}a{text-decoration:none}li{margin:4px 0}</style>");
    html.push_str("</head><body>");
    html.push_str("<h1>Index of ");
    html.push_str(&escape_html(&base_prefix));
    html.push_str("</h1><ul>");

    if !relative.as_os_str().is_empty() {
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let parent_href = if parent.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parent.to_string_lossy())
        };
        html.push_str("<li><a href=\"");
        html.push_str(&escape_html(&parent_href));
        html.push_str("\">..</a></li>");
    }

    for (name, is_dir) in rows {
        let mut href = if base_prefix.is_empty() {
            format!("/{}", name)
        } else {
            format!("{}/{}", base_prefix, name)
        };
        let mut label = name;
        if is_dir {
            href.push('/');
            label.push('/');
        }

        html.push_str("<li><a href=\"");
        html.push_str(&escape_html(&href));
        html.push_str("\">");
        html.push_str(&escape_html(&label));
        html.push_str("</a></li>");
    }

    html.push_str("</ul></body></html>");
    Ok(html)
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

fn server_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}
