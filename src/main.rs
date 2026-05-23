use std::env;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use bytes::Bytes;
use futures_util::stream::Stream;
use tower::ServiceExt;
use tower_http::services::ServeFile;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let port_arg = match args.next() {
        Some(value) => value,
        None => return usage("missing port argument"),
    };
    let root_arg = args.next();
    if args.next().is_some() {
        return usage("too many arguments");
    }

    let port: u16 = match port_arg.parse() {
        Ok(value) => value,
        Err(_) => return usage("invalid port"),
    };
    let root = match root_arg {
        Some(value) => {
            let root = PathBuf::from(value);
            let metadata = match std::fs::metadata(&root) {
                Ok(value) => value,
                Err(_) => return usage("root path does not exist or is not accessible"),
            };
            if !metadata.is_dir() {
                return usage("root path is not a directory");
            }
            match root.canonicalize() {
                Ok(value) => Some(value),
                Err(_) => return usage("failed to canonicalize root path"),
            }
        }
        None => None,
    };

    let addr: SocketAddr = format!("[::]:{}", port).parse()?;
    let state = AppState { root };
    let app = Router::new()
        .route("/", get(redirect_root))
        .route("/file", get(handle_file))
        .route("/sytle.css", get(handle_style_css))
        .route("/app.js", get(handle_app_js))
        .route("/speedtest", get(handle_speedtest))
        .with_state(state);

    println!("Serving on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn usage(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Error: {message}");
    eprintln!("Usage: relay <port> [root_path]");
    Err("invalid arguments".into())
}

#[derive(Clone)]
struct AppState {
    root: Option<PathBuf>,
}

struct TemplateContext {
    vars: HashMap<&'static str, String>,
    sections: HashMap<&'static str, Vec<HashMap<&'static str, String>>>,
}

const LISTING_TEMPLATE: &str = include_str!("../templates/listing.html");
const SPEEDTEST_TEMPLATE: &str = include_str!("../templates/speedtest.html");
const STYLE_CSS: &str = include_str!("../templates/sytle.css");
const APP_JS: &str = include_str!("../templates/app.js");

impl TemplateContext {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
            sections: HashMap::new(),
        }
    }
}

async fn redirect_root() -> Redirect {
    Redirect::to("/file?loc=/")
}

async fn handle_file(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    req: Request<Body>,
) -> Result<Response, Response> {
    if state.root.is_none() {
        let html = match render_speedtest_only().await {
            Ok(value) => value,
            Err(_) => return Err(server_error()),
        };
        return Ok(Html(html).into_response());
    }

    let loc = params.get("loc").map(String::as_str).unwrap_or("/");
    let path = match normalize_loc(loc) {
        Some(value) => value,
        None => return Err(not_found()),
    };
    handle_path_impl(state, path, req).await
}

async fn handle_path_impl(
    state: AppState,
    path: String,
    req: Request<Body>,
) -> Result<Response, Response> {
    if state.root.is_none() {
        return Err(not_found());
    }
    let relative = sanitize_path(&path).ok_or_else(not_found)?;
    let full_path = state.root.as_ref().unwrap().join(&relative);

    let metadata = match tokio::fs::metadata(&full_path).await {
        Ok(value) => value,
        Err(_) => return Err(not_found()),
    };

    if metadata.is_dir() {
        let html = match render_directory_listing(state.root.as_ref().unwrap(), &relative).await {
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

async fn handle_speedtest() -> Response {
    let stream = speedtest_stream();
    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    response
}

async fn handle_style_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
        .into_response()
}

async fn handle_app_js() -> Response {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        APP_JS,
    )
        .into_response()
}

fn speedtest_stream() -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::stream! {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            fastrand::fill(&mut buffer);
            yield Ok(Bytes::copy_from_slice(&buffer));
            tokio::task::yield_now().await;
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

fn normalize_loc(loc: &str) -> Option<String> {
    if loc.is_empty() || loc == "/" {
        return Some(String::new());
    }
    let stripped = loc.strip_prefix('/')?;
    Some(stripped.to_string())
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

    let mut items = Vec::new();

    if !relative.as_os_str().is_empty() {
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let parent_href = if parent.as_os_str().is_empty() {
            "/file?loc=/".to_string()
        } else {
            format!("/file?loc=/{}/", parent.to_string_lossy())
        };
        items.push(HashMap::from([
            ("href", parent_href),
            ("label", "..".to_string()),
        ]));
    }

    for (name, is_dir) in rows {
        let mut href = if base_prefix.is_empty() {
            format!("/file?loc=/{}", name)
        } else {
            format!("/file?loc={}/{}", base_prefix, name)
        };
        let mut label = name;
        if is_dir {
            href.push('/');
            label.push('/');
        }
        items.push(HashMap::from([("href", href), ("label", label)]));
    }

    let page_title = if base_prefix.is_empty() {
        "Index of /".to_string()
    } else {
        format!("Index of {}", base_prefix)
    };
    let heading = page_title.clone();

    let mut ctx = TemplateContext::new();
    ctx.vars.insert("title", page_title);
    ctx.vars.insert("heading", heading);
    ctx.sections.insert("items", items);
    render_template("listing.html", &ctx).await
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

async fn render_speedtest_only() -> Result<String, std::io::Error> {
    let mut ctx = TemplateContext::new();
    ctx.vars.insert("title", "Speed Test".to_string());
    ctx.vars.insert("heading", "Speed Test".to_string());
    ctx.vars.insert(
        "description",
        "点击右下角按钮开始测试当前连接下载速度。".to_string(),
    );
    render_template("speedtest.html", &ctx).await
}

async fn render_template(name: &str, ctx: &TemplateContext) -> Result<String, std::io::Error> {
    let template = match name {
        "listing.html" => LISTING_TEMPLATE,
        "speedtest.html" => SPEEDTEST_TEMPLATE,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "template not found",
            ));
        }
    };
    Ok(apply_template(template, ctx))
}

fn apply_template(template: &str, ctx: &TemplateContext) -> String {
    let with_sections = render_sections(template, &ctx.sections);
    render_vars(&with_sections, &ctx.vars)
}

fn render_sections(
    template: &str,
    sections: &HashMap<&'static str, Vec<HashMap<&'static str, String>>>,
) -> String {
    let mut output = template.to_string();
    for (name, rows) in sections {
        let start_tag = format!("{{{{#{name}}}}}");
        let end_tag = format!("{{{{/{name}}}}}");

        while let Some(start) = output.find(&start_tag) {
            let inner_start = start + start_tag.len();
            let Some(relative_end) = output[inner_start..].find(&end_tag) else {
                break;
            };
            let end = inner_start + relative_end;
            let block = &output[inner_start..end];

            let mut rendered = String::new();
            for row in rows {
                rendered.push_str(&render_vars(block, row));
            }

            let replace_end = end + end_tag.len();
            output.replace_range(start..replace_end, &rendered);
        }
    }
    output
}

fn render_vars(template: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut output = template.to_string();
    for (key, value) in vars {
        let raw_token = format!("{{{{{{{key}}}}}}}");
        output = output.replace(&raw_token, value);
        let token = format!("{{{{{key}}}}}");
        output = output.replace(&token, &escape_html(value));
    }
    output
}
