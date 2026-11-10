# 0014 版本更新

状态：已采纳

## 背景

原来要更新只能自己去 GitHub 下载新的安装包，关于页的“检查更新”只弹一句“即将支持”。
ScreenFerry 的情况有几处特别：

- 两台电脑都要装，版本最好一致；新版本改了线协议时，两台不一起更新就连不上。
- macOS 有 Developer ID，签名、公证都做得了；Windows 不签名（ADR 0011），装在 Program Files，
  装新版本要管理员权限。
- 从 Mac 控制 Windows 时，Windows 的 UAC 确认框在安全桌面上，远程的键鼠点不到。
- 目标是低内存，后台不能为更新多常驻一个进程。

## 调研

| 做法 | 长处 | 放到这里的问题 |
|---|---|---|
| Sparkle 2 + WinSparkle | Mac 上的事实标准，EdDSA 签名加 Apple 签名双重校验，有增量包 | 两套系统、两套界面；Sparkle 是 Objective-C 框架，要塞进 Rust 后台；WinSparkle 每次弹 UAC |
| Velopack | Rust 写的一体化框架，zstd 增量包 | 换掉 Inno Setup、默认装进用户目录，和 ADR 0011 冲突；crate 还是 0.0.x |
| 签名清单（Tauri updater 的做法） | 一份静态 JSON 加 minisign 签名，放在 GitHub Releases 上就行 | 只防篡改，不防回滚、冻结 |
| TUF（Rust 的 tough） | 最完整的更新安全模型 | 一个人维护太重；tough 2025 年还修过回滚检测的漏洞 |
| Sigstore、GitHub 构建证明 | 不用自己保管密钥，有透明日志 | 程序离线校验要带完整证明链，库太大 |
| 提权更新服务（Chrome、Firefox） | 装好后更新不再弹 UAC | 多一个高权限组件 |

## 决定

自己写一个轻量的更新器：签名清单的格式，加上 TUF 最关键的三条规则，安装交给各平台原生的做法。

### 发布

- `cargo xtask release <版本>` 改版本号、整理 CHANGELOG，检查 `docs/releases/<版本>.md` 里的中英文更新说明，
  提交并打 tag。推送由人来做。
- 推上 tag 后 CI（`release.yml`）构建 Windows 安装包和 macOS 通用版（arm64 + x86_64，签名、公证、装订），
  写 `update.json` 并用发布私钥签名，附上 GitHub 构建证明，存成草稿。维护者看过后点“公开”。
- 没有配置 macOS 的签名和公证密钥时，这次发布只含 Windows 安装包。

### 版本号、更新说明和发布产物

- 版本号：只有修复，升最后一位（0.2.0 → 0.2.1）；有新功能、改进，或者改了两台之间的通信方式，升中间一位
  （0.2.x → 0.3.0）。还没发过的当前版本可以直接发（第一版）。
- 更新说明 `docs/releases/<版本>.md`：`## 简体中文`、`## English` 两节，每节按 `### 新增 / 改进 / 修复`
  （`New / Improved / Fixed`）分类，每条一行、以“- ”开头，没有内容的分类不写。草稿正文就是这个文件；
  写进清单时保留分类，程序的更新卡片按分类显示。只写用户看得懂的几条，细节写在 CHANGELOG。
- 每个版本的附件 5 个，名字是 `ScreenFerry-<版本>-<系统>-<架构>.<扩展名>`：
  `ScreenFerry-0.2.0-macos-universal.dmg`（第一次安装）、`ScreenFerry-0.2.0-macos-universal.zip`（自动更新）、
  `ScreenFerry-0.2.0-windows-x64-setup.exe`，加上 `update.json`、`update.json.minisig`。不另附 SHA-256 文件：
  每个包的 SHA-256 写在签名的清单里。

### Mac 安装盘

- 窗口 660 × 400（加标题栏），背景是 `assets/dmg/background.svg` 导出的普通和 Retina 两张图：上方一条品牌色带，
  中间两个图标之间三个渐深的箭头，下方一句中英文说明。图标 128、文件名 13。
- 图标下的文件名由访达来画、颜色由系统决定，那一块保持浅色；打开了“显示路径栏”的访达会盖住最下面 28，那里不放东西。
- dmgbuild 直接写窗口布局，不用打开访达；压缩成 ULFO，签名、公证、装订。

### 清单

`update.json`（`ferry-update` 定义）：`schema`、`version`、`published`、`expires`（发布后 180 天）、`protocol`（线协议版本）、
`notes`（中英文，每行一条）、`assets`（`macos-universal`、`windows-x86_64` 各自的地址、大小、SHA-256）。
签名在 `update.json.minisig`。程序从 `releases/latest/download/update.json` 取，走下载地址，不受 API 每小时 60 次的限制。

安全规则：

- 程序内置两把 minisign 公钥（`crates/ferry-update/src/keys.rs`）：日常那把的私钥加口令放在 CI 的 Secrets 里，
  备用那把离线保存。日常那把泄露时，用备用那把签一个新版本，新版本里换掉公钥列表。
  这是“轮换”：只加不删会让泄露的那把一直有效，所以换的时候去掉它。
- 清单过期了，不说“已是最新”，提示去下载页面看（防冻结）。
- 发布时间早于见过的那份（记在 `updates/state.toml`）不认（防回滚）；只装比自己新的版本。
- 下载的文件大小和 SHA-256 必须和清单一致；macOS 另要求新应用的签名满足本机这份的指定要求、并通过 Gatekeeper（已公证）。
- 请求只带 `ScreenFerry/<版本> (<平台>)` 这个 User-Agent，不带设备标识。证书用系统的校验，代理用系统设置里的。

### 检查和下载

- 设置“自动检查更新”（`auto_update`，缺省开着，配置升到 `schema_version = 6`）：启动两分钟后查一次，之后每天一次，
  时间带随机偏移；有新版本就在后台下载好。关着时只在用户点“检查更新”时查，发现了也等用户点“下载”。
- 自动检查连不上服务器时不打扰用户，一小时后再试；用户点的检查出错才显示。
- 更新包存在缓存目录的 `updates/` 里，边下边写盘，断了接着下。这个目录可以随时清空。

### 安装

- 自动下载，不自动安装：装的时候键鼠要断几秒，由用户点“重启并更新”（关于页、仪表盘横幅、侧栏、托盘菜单）。
- 装之前在 `updates/state.toml` 记下“从哪个版本装到哪个版本”，新版本启动后据此提示“已更新到 …”，
  安装前设置界面开着的话重新打开它。
- macOS：解压到应用旁边的隐藏文件夹，核对签名和公证，用 `renamex_np(RENAME_SWAP)` 一步交换新旧应用；
  本进程停下共享、让出 `ipc.json`，启动新版本，20 秒内新版本没写下 `ipc.json` 就换回旧版本再启动。
  签名、bundle id 和位置不变，辅助功能权限不会丢。临时签名的开发版本不能自己更新。
- Windows：后台本身已经是管理员权限，直接静默运行安装包，不再弹 UAC（从普通权限进程更新时才弹一次），
  安装包请后台退出、装完以管理员权限重新启动（ADR 0011、ADR 0015）。
  程序自己下载的文件没有“来自网络”的标记，SmartScreen 不会拦；从核对完到安装包运行，一直独占着文件，中途换不掉。
  这台被另一台电脑控制时，界面提醒用这台自己的键鼠确认。不是用安装包装的（开发时直接运行）不能自己更新。

### 两台一起

- 线协议加能力位 `APP_INFO` 和消息 `AppInfo { version }`（`0x0106`），握手后互相告诉版本；旧版本照常能连，
  协议版本号不变。设备页显示对方的版本，对方比本机知道的最新版本旧时提示可以更新。
- 新版本的 `protocol` 和本机不同时，更新卡片提醒两台要一起更新。

### 本机通信

只加字段和命令，版本仍是 2：`state.update`、`settings.auto_update`、`peers[].version`，
命令 `check_update`、`download_update`、`cancel_update`、`install_update`、`dismiss_update`，
活动记录和提示的 `phrase` 新增 `update_installed`、`update_not_installed`、`update_failed`、`update_cancelled`。

### 目录

缓存目录新增 `updates/`（AGENTS.md 第 10 节）；新增 L1 crate `ferry-update`（第 4 节），只做清单、验签和判断，不做网络和文件读写；
`docs/releases/` 放每个版本的更新说明。

## 不做（第二期）

- 请对方也更新、在局域网里代传安装包（对方连不上外网时）：清单有签名，代传不影响可信。
- Windows 免确认更新：安装时登记一个提权的计划任务，只肯装签名有效的新版本，像 Firefox 的 Maintenance Service。
  要先想清楚安全边界。
- 测试版渠道、增量更新包（现在整包约 21 MB）。
