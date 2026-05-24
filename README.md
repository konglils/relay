# relay

一个轻量的跨平台局域网文件分享与测速工具。

## 主要功能

- 浏览并下载指定根目录下的文件
- 内置带宽测速接口：`/speedtest`
- Web 界面支持二维码分享
- 单二进制部署（HTML/CSS/JS 在编译期内嵌）
- 启动时打印可分享地址，优先 IPv6

## 编译

推荐使用 `musl` 进行静态构建，便于分发：

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

二进制路径：

```bash
./target/x86_64-unknown-linux-musl/release/relay
```

也可以使用默认目标编译：

```bash
cargo build --release
```

## 使用方法

```bash
relay <port> [root_path]
```

示例：

```bash
# 文件分享 + 测速
relay 8000 /path/to/share

# 仅测速页面
relay 8000
```

启动后访问：

- `http://<host>:<port>/`（会重定向到 `/file?loc=/`）

程序路由：

- `/`：重定向到文件浏览入口
- `/file?loc=/...`：文件/目录浏览
- `/speedtest`：测速数据流

## 注意事项

- `root_path` 必须存在且为目录。
- 服务端会对路径做安全清洗，拒绝目录穿越（如 `..`）。

## 安全说明

- 本项目不提供传输加密（无 HTTPS/TLS），请勿在公网环境分享重要或私有文件。
- 建议仅在可信局域网中使用；如需公网使用，请自行在反向代理层启用 HTTPS 和访问控制。

## AI 说明

- 本项目代码 99% 由 AI 生成。
