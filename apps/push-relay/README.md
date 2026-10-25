# runode-push

runode 官方 iOS App（`dev.runode.mobile`）的推送中转，跑在 `https://push.runode.dev`。

手机拿到 APNs device token 后，为每台电脑调一次 `POST /v1/register` 换一个句柄，只把句柄交给电脑。电脑要推通知时调 `POST /v1/notify` 把句柄交回来，中转解开句柄、核对电脑，再用 Apple 给的密钥签名发给 APNs。电脑始终拿不到 device token，句柄泄露了也只能替原来那台电脑推，而且按句柄限频。请求体、回话和错误码以 `runode_protocol::push` 为准，改那边的中转格式时同一个提交里改这里。

中转不存状态：句柄就是用中转自己的密钥（AES-256-GCM）封起来的 token、APNs 环境和电脑 UUID，限频计数只在内存里，重启就清零。日志里只记状态码和耗时，不记 token、句柄和通知内容，隐私政策里是这么承诺的。

## 构建

它是 runode workspace 的成员，但不在 `default-members` 里，不带 `-p` 的 cargo 命令编不到它。在仓库根目录：

```sh
cargo test -p runode-push
cargo build --release --locked -p runode-push   # 产物在 target/release/runode-push
```

`make test`、`make clippy` 带着 `--workspace`，CI 里会一起测。

## 配置

都从环境变量读：

| 变量 | 说明 |
| --- | --- |
| `RELAY_HANDLE_KEYS` | 封句柄的密钥，逗号隔开的 `<编号>=<密钥>`，编号 0 到 255。第一把用来封新句柄，每一把都能解。用 `runode-push keygen` 生成。 |
| `APNS_KEY_FILE` | Apple 开发者后台下载的 `AuthKey_<ID>.p8`。 |
| `APNS_KEY_ID` | 这把 .p8 的 key ID。 |
| `APNS_TEAM_ID` | 开发者账号的 team ID。 |
| `RELAY_LISTEN` | 监听地址，默认 `127.0.0.1:8080`。 |
| `RELAY_TRUST_PROXY` | 设成 `1` 时，按 `X-Forwarded-For` 最右边那个地址限频，在反向代理后面必须设。 |
| `RUST_LOG` | 日志级别，默认 `info`。 |

## 部署

下面以 systemd 加 Caddy 为例，Caddy 负责 HTTPS：

```sh
sudo install -m 755 target/release/runode-push /usr/local/bin/
sudo install -d -m 700 /etc/runode-push
sudo install -m 600 AuthKey_XXXXXXXXXX.p8 /etc/runode-push/AuthKey.p8
sudo tee /etc/runode-push/env >/dev/null <<EOF
RELAY_HANDLE_KEYS=1=$(runode-push keygen)
APNS_KEY_ID=XXXXXXXXXX
APNS_TEAM_ID=YYYYYYYYYY
EOF
sudo chmod 600 /etc/runode-push/env
sudo install -m 644 apps/push-relay/deploy/runode-push.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now runode-push
```

把 `apps/push-relay/deploy/Caddyfile` 合进 Caddy 的配置，reload 一下。之后用 `journalctl -u runode-push -f` 看日志。

## 换密钥

- **句柄密钥：** 生成一把新的、换一个编号，放到 `RELAY_HANDLE_KEYS` 最前面，旧的留在后面，然后重启服务。手机每次连上电脑都会重新换句柄，等大家都换过了（留一两周），再删掉旧的。旧密钥删掉以后，还拿着旧句柄的电脑会收到 400 `BadDeviceToken`，删掉那条登记，等手机下次连上时再重新登记。
- **APNs 的 .p8：** 在 Apple 开发者后台新建一把，替换 `/etc/runode-push/AuthKey.p8`，改好 `APNS_KEY_ID` 后重启服务，确认能推了再去后台吊销旧的。
