# 0004. 设置界面直连后台，后台常驻

- 状态：已采纳（2026-11-01，维护者委托选择，要求稳定、高性能、兼容 Windows 与 macOS）
- 日期：2026-11-01
- 取代：ADR 0003 中“经 flutter_rust_bridge 调用 `ferry-settings`”的部分

## 背景

ADR 0003 计划把 `ferry-settings` 编成动态库，经 flutter_rust_bridge 给 Flutter 调用。落地时发现：

- 导出 C 接口需要 `#[unsafe(no_mangle)]`，违反第 5 节“`unsafe` 只允许出现在 `ferry-platform-*` 中”。
- 要引入第三方 Dart 包和代码生成器，两个平台都要把 Rust 动态库打进 Flutter 应用。
- `dart:io` 的 Unix domain socket 不支持 Windows，Dart 也没有命名管道，界面无法直接走原定的本机通信方式。

| 方案 | 优点 | 缺点 |
|---|---|---|
| flutter_rust_bridge（原计划） | 消息类型只在 Rust 定义一次 | 破例允许 `unsafe`；第三方包与代码生成；动态库打包最复杂 |
| dart:ffi 手写薄接口 | 不引入第三方包 | 同样要破例 `unsafe`、打包动态库；Dart 侧消息仍要手写 |
| 界面直连后台的本机回环端口 | `dart:io` 自带，两个平台同一套代码；无动态库、无 `unsafe`、无第三方包；两个进程互不拖垮 | 消息类型 Rust 与 Dart 各写一份；回环端口须靠令牌鉴权 |

## 决策

### 本机通信

- 后台监听 `127.0.0.1` 的随机端口，每次启动生成 32 字节随机令牌，把端口、令牌、进程号写入缓存目录的 `ipc.json`。缓存目录只有当前用户能访问。
- 界面读 `ipc.json` 连接，第一条消息带上令牌；令牌不符立即断开。
- 每行一条 JSON（UTF-8，单行不超过 1 MiB）。协议版本 1，消息定义在 `ferry-ipc`，说明见 `docs/ipc.md`。
- 后台在状态变化时推送完整的状态快照（最多每秒 4 次），另推日志、配对请求等事件；界面只发命令，不保存业务状态。
- Dart 侧手写对应的类型；`ferry-ipc` 的测试生成样例 JSON，界面的测试读取同一批样例，保证两边一致。
- `ferry-settings` 删除，界面不再有 Rust 侧。

### 后台常驻

- `screenferry` 不带参数时常驻运行：读 `config.toml`、启动引擎、提供本机通信、显示托盘图标。`serve`、`connect`、`peers`、`forget` 保留，用于调试。
- 只运行一份：启动时 `ipc.json` 指向的后台还活着，就请它打开设置界面，然后退出。
- 首次配对在界面上核对配对码。没有界面连着时，后台先打开设置界面。
- 引擎对外报告连接、断开、往返延迟、光标进出、剪贴板同步、配对结果。屏幕方向、⌘ 与 Ctrl 互换即时生效；其余设置改变时重启引擎，连接约一秒内恢复。暂停共享即停止引擎。
- 托盘用 tray-icon。主线程运行平台事件循环（`ferry-platform` 新增 `EventLoop`），tokio 在另一个线程。
- 开机自启：macOS 写 LaunchAgent；Windows 写当前用户的“登录时启动、最高权限”任务计划程序任务。
  旧版本的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 登记会自动迁移。是否开启以系统中的登记为准，不写进配置。

### 配置

`config.toml`，只有后台写入：

```toml
schema_version = 1
name = ""             # 设备名，空为计算机名
role = "server"       # server：键鼠在本机；client：被控制。缺省时 macOS 为 server，Windows 为 client
side = "right"        # server：对方屏幕在本机哪一侧
swap_cmd_ctrl = true  # server：Command 当作对方的 Ctrl
announce = true       # server：在局域网里广播本机
clipboard = true      # 同步剪贴板
server = ""           # client：服务端地址，空为在局域网里寻找
theme = "system"      # 设置界面配色：system、light、dark
accent = 0            # 设置界面强调色，界面预设颜色的序号
```

界面的配色也存在这里：界面不直接写文件，改配色同样经后台保存。

## 对规范的修改

- 第 2 节：“本机进程通信”改为“本机回环 TCP + 令牌”；“界面”一行去掉 flutter_rust_bridge 与 `ferry-settings`。
- 第 4 节：删除 `ferry-settings`。
- 第 5 节第 4 条：改为“设置界面只通过 `ferry-ipc` 定义的协议与后台通信，不直接读写配置；配置只有后台一个写入方”。
- 第 8 节：新增 `event_loop.rs`（`EventLoop`）。
- 第 10 节：缓存目录新增 `ipc.json`。

## 后果

- 设置界面与后台之间没有 FFI、动态库和第三方 Dart 包，两个平台走同一条路径。
- 消息类型两边各有一份，靠共享样例保持一致；改协议时两边一起改。
- 回环端口本机其他用户也能连上，靠令牌和缓存目录的访问权限挡住；后台只监听 `127.0.0.1`。
- Windows 版设置界面须在 Windows 上构建（Flutter 不能在 macOS 上交叉编译 Windows 应用）。
