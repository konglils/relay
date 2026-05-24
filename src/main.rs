use std::env;
use std::collections::HashMap;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

    print_access_urls(port);

    let listener_v6 = tokio::net::TcpListener::bind(addr).await?;

    #[cfg(target_os = "windows")]
    {
        let addr_v4 = SocketAddr::from(([0, 0, 0, 0], port));
        match tokio::net::TcpListener::bind(addr_v4).await {
            Ok(listener_v4) => {
                let app_v6 = app.clone();
                tokio::try_join!(
                    axum::serve(listener_v6, app_v6),
                    axum::serve(listener_v4, app)
                )?;
            }
            Err(_) => {
                axum::serve(listener_v6, app).await?;
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        axum::serve(listener_v6, app).await?;
    }
    Ok(())
}

fn usage(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Error: {message}");
    eprintln!("Usage: relay <port> [root_path]");
    Err("invalid arguments".into())
}

fn print_access_urls(port: u16) {
    let ifaces = collect_interface_ips();
    let temporary_ipv6 = linux_temporary_ipv6_set();

    let mut urls = Vec::new();
    for iface in ifaces {
        let ip = iface.ip;
        if !is_shareable_ip(ip, temporary_ipv6.as_ref()) {
            continue;
        }
        let url = match ip {
            IpAddr::V4(v4) => format!("http://{}:{port}", v4),
            IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
        };
        urls.push((ip_priority(ip), url));
    }

    urls.sort_by_key(|(priority, url)| (*priority, url.clone()));
    urls.dedup_by(|a, b| a.1 == b.1);

    if urls.is_empty() {
        return;
    }

    println!("Available sharing URLs:");
    for (_, url) in urls {
        println!("  {url}");
    }
}

#[derive(Clone, Debug)]
struct InterfaceIp {
    ip: IpAddr,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn collect_interface_ips() -> Vec<InterfaceIp> {
    use std::ffi::CStr;

    let mut out = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();

    // SAFETY: libc guarantees `getifaddrs` initializes `ifap` on success.
    let rc = unsafe { libc::getifaddrs(&mut ifap) };
    if rc != 0 || ifap.is_null() {
        return out;
    }

    let mut cur = ifap;
    while !cur.is_null() {
        // SAFETY: `cur` is a valid node from the linked list returned by `getifaddrs`.
        let ifa = unsafe { &*cur };
        if !ifa.ifa_addr.is_null() {
            let flags = ifa.ifa_flags as i32;
            let is_up = (flags & libc::IFF_UP) != 0;
            let is_loopback = (flags & libc::IFF_LOOPBACK) != 0;

            if is_up && !is_loopback {
                // SAFETY: `ifa_name` is a valid NUL-terminated C string for each entry.
                let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                    .to_string_lossy()
                    .into_owned();
                if is_shareable_interface_unix(&name) {
                    // SAFETY: `ifa_addr` points to a sockaddr with family-dispatched layout.
                    let family = unsafe { (*ifa.ifa_addr).sa_family as i32 };
                    match family {
                        libc::AF_INET => {
                            // SAFETY: family is AF_INET so cast is valid.
                            let sa = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                            let ip = Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                            out.push(InterfaceIp { ip: IpAddr::V4(ip) });
                        }
                        libc::AF_INET6 => {
                            // SAFETY: family is AF_INET6 so cast is valid.
                            let sa = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                            let ip = Ipv6Addr::from(sa.sin6_addr.s6_addr);
                            out.push(InterfaceIp { ip: IpAddr::V6(ip) });
                        }
                        _ => {}
                    }
                }
            }
        }
        cur = ifa.ifa_next;
    }

    // SAFETY: `ifap` was returned by `getifaddrs`.
    unsafe { libc::freeifaddrs(ifap) };
    out
}

#[cfg(target_os = "linux")]
fn is_shareable_interface_unix(name: &str) -> bool {
    let sys_path = Path::new("/sys/class/net").join(name);
    let device_path = sys_path.join("device");
    let virtual_path = Path::new("/sys/devices/virtual/net").join(name);
    device_path.exists() && !virtual_path.exists()
}

#[cfg(target_os = "android")]
fn is_shareable_interface_unix(_name: &str) -> bool {
    // Android 上很多可用接口在 sysfs 中显示为 virtual，不能用 Linux 桌面那套物理设备规则过滤。
    true
}

#[cfg(target_os = "windows")]
fn collect_interface_ips() -> Vec<InterfaceIp> {
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_ADDRESS_TRANSIENT,
    };
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, IpDadStatePreferred, SOCKADDR_IN, SOCKADDR_IN6,
    };

    const IF_OPER_STATUS_UP: i32 = 1;
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
    const IF_TYPE_TUNNEL: u32 = 131;

    let mut out = Vec::new();
    let mut buflen: u32 = 16 * 1024;
    let mut buf: Vec<u8> = vec![0; buflen as usize];

    // SAFETY: We pass a valid mutable byte buffer and size pointer per API contract.
    let mut ret = unsafe {
        GetAdaptersAddresses(
            AF_UNSPEC as u32,
            GAA_FLAG_SKIP_ANYCAST
                | GAA_FLAG_SKIP_MULTICAST
                | GAA_FLAG_SKIP_DNS_SERVER,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
            &mut buflen,
        )
    };

    if ret == ERROR_BUFFER_OVERFLOW {
        buf.resize(buflen as usize, 0);
        // SAFETY: Buffer was resized to requested length from previous call.
        ret = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_SKIP_ANYCAST
                    | GAA_FLAG_SKIP_MULTICAST
                    | GAA_FLAG_SKIP_DNS_SERVER,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut buflen,
            )
        };
    }

    if ret != NO_ERROR {
        return out;
    }

    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !adapter.is_null() {
        // SAFETY: `adapter` is part of the linked list returned by the API.
        let ad = unsafe { &*adapter };
        let friendly = wchar_ptr_to_string(ad.FriendlyName);
        let is_up = ad.OperStatus == IF_OPER_STATUS_UP;
        let allowed_type = ad.IfType != IF_TYPE_SOFTWARE_LOOPBACK && ad.IfType != IF_TYPE_TUNNEL;
        let not_virtual = !looks_virtual_windows_interface(&friendly);
        if is_up && allowed_type && not_virtual {
            let mut uni = ad.FirstUnicastAddress;
            while !uni.is_null() {
                // SAFETY: `uni` is a node in adapter's unicast linked list.
                let u = unsafe { &*uni };
                let sa = u.Address.lpSockaddr;
                if !sa.is_null() {
                    // SAFETY: `sa` points to a valid sockaddr.
                    let family = unsafe { (*sa).sa_family };
                    match family {
                        AF_INET => {
                            // SAFETY: family-dispatched cast.
                            let s4 = unsafe { &*(sa as *const SOCKADDR_IN) };
                            // SAFETY: reading union field for IPv4 byte view.
                            let oct = unsafe { s4.sin_addr.S_un.S_un_b };
                            let ip = Ipv4Addr::new(oct.s_b1, oct.s_b2, oct.s_b3, oct.s_b4);
                            out.push(InterfaceIp { ip: IpAddr::V4(ip) });
                        }
                        AF_INET6 => {
                            // SAFETY: family-dispatched cast.
                            let s6 = unsafe { &*(sa as *const SOCKADDR_IN6) };
                            // SAFETY: reading union field for IPv6 byte view.
                            let ip = Ipv6Addr::from(unsafe { s6.sin6_addr.u.Byte });
                            // Prefer temporary IPv6 addresses only; also exclude tentative/deprecated.
                            // SAFETY: field access through documented union layout.
                            let flags = unsafe { u.Anonymous.Anonymous.Flags };
                            let is_transient = (flags & IP_ADAPTER_ADDRESS_TRANSIENT) != 0;
                            let dad_preferred = u.DadState == IpDadStatePreferred;
                            let not_expired = u.PreferredLifetime > 0;
                            if is_transient && dad_preferred && not_expired {
                                out.push(InterfaceIp { ip: IpAddr::V6(ip) });
                            }
                        }
                        _ => {}
                    }
                }
                uni = u.Next;
            }
        }
        adapter = ad.Next;
    }

    out
}

#[cfg(target_os = "windows")]
fn wchar_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: `ptr` is NUL-terminated UTF-16 from Windows API.
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

#[cfg(target_os = "windows")]
fn looks_virtual_windows_interface(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let hints = [
        "virtual",
        "hyper-v",
        "vmware",
        "virtualbox",
        "vbox",
        "wsl",
        "tap",
        "tun",
        "tailscale",
        "zerotier",
        "docker",
        "loopback",
        "vpn",
    ];
    hints.iter().any(|h| lower.contains(h))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "windows")))]
fn collect_interface_ips() -> Vec<InterfaceIp> {
    Vec::new()
}

fn ip_priority(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V6(v6) if is_public_ipv6(v6) => 0,
        IpAddr::V4(v4) if is_public_ipv4(v4) => 1,
        IpAddr::V4(_) => 2,
        IpAddr::V6(_) => 3,
    }
}

fn is_shareable_ip(ip: IpAddr, temporary_ipv6: Option<&HashSet<Ipv6Addr>>) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast())
        }
        IpAddr::V6(v6) => {
            if !is_public_ipv6(v6) {
                return false;
            }
            if let Some(temp_set) = temporary_ipv6 {
                return temp_set.contains(&v6);
            }
            true
        }
    }
}

fn is_public_ipv4(v4: Ipv4Addr) -> bool {
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_unspecified()
        || v4.is_documentation()
        || v4.octets()[0] == 0)
}

fn is_public_ipv6(v6: Ipv6Addr) -> bool {
    !(v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || v6.is_unique_local()
        || v6.is_unicast_link_local())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_temporary_ipv6_set() -> Option<HashSet<Ipv6Addr>> {
    const IFA_F_TEMPORARY: u32 = 0x01;
    const IFA_F_DEPRECATED: u32 = 0x20;
    const IFA_F_TENTATIVE: u32 = 0x40;

    let content = std::fs::read_to_string("/proc/net/if_inet6").ok()?;
    let mut set = HashSet::new();

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(addr_hex) = parts.next() else { continue };
        let _index = parts.next();
        let _prefix = parts.next();
        let scope_hex = parts.next();
        let flags_hex = parts.next();
        let _if_name = parts.next();

        if addr_hex.len() != 32 {
            continue;
        }
        let Some(scope_hex) = scope_hex else { continue };
        let Some(flags_hex) = flags_hex else { continue };

        let Ok(scope) = u32::from_str_radix(scope_hex, 16) else {
            continue;
        };
        if scope != 0 {
            continue;
        }

        let Ok(flags) = u32::from_str_radix(flags_hex, 16) else {
            continue;
        };
        let is_temporary = (flags & IFA_F_TEMPORARY) != 0;
        let is_deprecated = (flags & IFA_F_DEPRECATED) != 0;
        let is_tentative = (flags & IFA_F_TENTATIVE) != 0;
        if !is_temporary || is_deprecated || is_tentative {
            continue;
        }

        let mut octets = [0u8; 16];
        let mut ok = true;
        for i in 0..16 {
            let start = i * 2;
            let end = start + 2;
            match u8::from_str_radix(&addr_hex[start..end], 16) {
                Ok(byte) => octets[i] = byte,
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        set.insert(Ipv6Addr::from(octets));
    }

    Some(set)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn linux_temporary_ipv6_set() -> Option<HashSet<Ipv6Addr>> {
    None
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
