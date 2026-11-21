# runode-push

runode 官方 App（iOS 和 Android 都是 `dev.runode.mobile`）的推送中转，是一个 Cloudflare Worker，跑在 `https://push.runode.dev`。

手机拿到推送用的 token（iPhone 是 APNs 的 device token，Android 是 FCM 的 registration token）后，为每台电脑调一次 `POST /v1/register` 换一个句柄，只把句柄交给电脑。电脑要推通知时调 `POST /v1/notify` 把句柄交回来，中转解开句柄、核对电脑，再用 Apple 给的密钥签名发给 APNs，或者用 Firebase 的服务账号发给 FCM。电脑始终拿不到 token，也不用分手机是哪个平台；句柄泄露了也只能替原来那台电脑推，而且按句柄限频。请求体、回话和错误码以 `runode_protocol::push` 为准，改那边的中转格式时同一个提交里改这里，测试逐字节核对 `runode-protocol` 的推送样例。

发给 FCM 的只有 `data`，通知由 Android App 自己用本地化的文案出。FCM 的回话换成电脑认得的 APNs 状态带回去：token 作废了（`UNREGISTERED`）回 410 `Unregistered`，token 不对（针对 token 的 `INVALID_ARGUMENT`、`SENDER_ID_MISMATCH`）回 400 `BadDeviceToken`，电脑见了都删掉登记；超了限额（`QUOTA_EXCEEDED`）回 429，带上 FCM 给的 `Retry-After`；别的照 FCM 的状态带回，原因是它的错误码。

中转不存状态：句柄就是用中转自己的密钥（AES-256-GCM）封起来的 token、发给谁（APNs 的哪个环境，或者 FCM）和电脑 UUID；限频用 Workers 的 Rate Limiting 绑定，计数在 Cloudflare 各个机房里各算各的，是大致的上限。日志里只记状态码和耗时，不记 token、句柄和通知内容，隐私政策里是这么承诺的；Workers Logs 另外会记每个请求的地址和方法，句柄和 token 只在请求体里，不在其中。

## 开发

```sh
cd apps/push-relay
npm ci
npm run check   # tsc，连测试代码一起查类型
npm test        # vitest
```

测试在 Node 里跑，出站请求和限频都是假的。本地的 `wrangler dev` 连不上 APNs（它只说 HTTP/2，本地的 workerd 发不出 HTTP/2 请求），要看真的回话只能部署上去。

## 配置

密钥都是 Worker 的 secret，用 `npx wrangler secret put <名字>` 设。单行的值可以等它提示后粘贴；`.p8` 和服务账号 JSON 是多行的，交互粘贴只会存进第一行，必须从文件灌进去：

| secret | 说明 |
| --- | --- |
| `RELAY_HANDLE_KEYS` | 封句柄的密钥，逗号隔开的 `<编号>=<密钥>`，编号 0 到 255，密钥是 32 字节的 base64url（不带 `=`）。第一把用来封新句柄，每一把都能解。生成一把：`openssl rand 32 \| base64 \| tr '+/' '-_' \| tr -d '='`。 |
| `APNS_KEY` | Apple 开发者后台下载的 `AuthKey_<ID>.p8` 的内容：`npx wrangler secret put APNS_KEY < AuthKey_XXXXXXXXXX.p8`。 |
| `APNS_KEY_ID` | 这把 .p8 的 key ID。 |
| `APNS_TEAM_ID` | 开发者账号的 team ID。 |
| `FCM_SERVICE_ACCOUNT` | 可选。Firebase 项目的服务账号 JSON 的内容（Firebase 控制台的「项目设置 → 服务账号 → 生成新的私钥」），要有发 FCM 消息的权限：`npx wrangler secret put FCM_SERVICE_ACCOUNT < fcm-service-account.json`。不设时不给 Android 推，Android 的登记回 400 `FcmNotConfigured`。 |

这几样有一样不对，中转就起不来，所有请求都回 500。限频的上限和域名在 `wrangler.jsonc` 里。

## 部署

`runode.dev` 这个 zone 要在同一个 Cloudflare 账号里。登录一次后在这个目录里：

```sh
npx wrangler login
npm run deploy
```

Worker 先跑在 `workers.dev` 的子域名上，还没绑 `push.runode.dev`。各端编进去的中转地址取自编译时的 `RUNODE_PUSH_RELAY`：发版和 TestFlight 的流水线读仓库的 Actions 变量 `RUNODE_PUSH_RELAY`，没设时编出来的包不带中转，换了地址要回来改它。之后用 `npx wrangler tail` 或控制台的 Workers Logs 看日志。

### 从 Rust 版的服务器切过来

1. 先设好上面的 secret，`RELAY_HANDLE_KEYS` 必须和老服务器 `/etc/runode-push/env` 里的一样，不然手机已经交给电脑的句柄全都解不开。Worker 还不存在时 `wrangler secret put` 会问要不要新建，答应即可。
2. 删掉 DNS 里 `push.runode.dev` 指向老服务器的记录（自定义域名不能和已有记录重名），马上 `npm run deploy`。
3. 用一台连着手机的电脑推一条通知，确认收到了，再停掉老服务器上的 `runode-push` 服务。

## 换密钥

- **句柄密钥：** 生成一把新的、换一个编号，放到 `RELAY_HANDLE_KEYS` 最前面，旧的留在后面，重新 `wrangler secret put`。手机每次连上电脑都会重新换句柄，等大家都换过了（留一两周），再删掉旧的。旧密钥删掉以后，还拿着旧句柄的电脑会收到 400 `BadDeviceToken`，删掉那条登记，等手机下次连上时再重新登记。
- **APNs 的 .p8：** 在 Apple 开发者后台新建一把，从文件重设 `APNS_KEY`，再改 `APNS_KEY_ID`，确认能推了再去后台吊销旧的。
- **FCM 的服务账号：** 在 Google Cloud 控制台给同一个服务账号新建一把密钥，从下载的 JSON 文件重设 `FCM_SERVICE_ACCOUNT`，确认 Android 能收到推送了再删掉旧的那把。换密钥不影响已经发出去的句柄。

secret 改了以后新的请求就用新值，不用重新部署。
