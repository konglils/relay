# relay

一个轻量的局域网文件分享与测速工具。

## 主要功能

- 浏览并下载指定根目录下的文件
- 内置带宽测速接口：`/speedtest`
- Web 界面支持二维码分享
- 单二进制部署（HTML/CSS/JS 在编译期内嵌）

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

- `http://<host>:<port>/`

## 注意事项

- `root_path` 必须存在且为目录。
- 服务端会对路径做安全清洗，拒绝目录穿越（如 `..`）。
