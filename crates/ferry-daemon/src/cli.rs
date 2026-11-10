//! 命令行参数。

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;

use ferry_core::Side;
use ferry_engine::DEFAULT_PORT;

pub(crate) const USAGE: &str = "\
用法：
  screenferry         [--data-dir 目录] [--autostart] [-v]
  screenferry serve   [--listen 地址] [--side 方向] [--no-cmd-ctrl-swap] [--no-announce] [通用选项]
  screenferry connect [服务端地址] [通用选项]
  screenferry peers   [--data-dir 目录]
  screenferry forget  <设备名或指纹> [--data-dir 目录]
  screenferry send    <文件或文件夹>... [--data-dir 目录]
  screenferry quit    [--data-dir 目录]

不带命令时常驻运行：按数据目录中的 config.toml 工作，菜单栏（任务栏）有图标，
配对、设置都在设置界面里完成。启动时会打开设置界面；--autostart 表示登录时由系统启动，
不打开。已经有一份在运行时，请它打开设置界面后退出。下面的命令用于调试，参数取代 config.toml。

命令：
  serve      在拥有键鼠的电脑上运行，等待另一台连接
  connect    在被控制的电脑上运行，连接到运行 serve 的那台。
             不填地址时在局域网里自动寻找：优先已配对的；从未配对过时，只有一台才自动连
  peers      列出已配对的设备
  forget     删除一台已配对的设备，下次连接需要重新配对
  send       请正在运行的 ScreenFerry 把文件或文件夹发给它连着的电脑，
             对方收到后放在“下载”文件夹
  quit       请正在运行的 ScreenFerry 退出，等它退出后返回（安装程序升级、卸载前用）

serve 的选项：
  --listen   监听地址，默认 0.0.0.0:24810
  --side     对方屏幕在本机的哪一侧：right、left、top、bottom，默认 right
  --no-cmd-ctrl-swap
             不互换 Command 与 Ctrl。默认互换，在 Windows 上 ⌘C、⌘V 即复制粘贴
  --no-announce
             不在局域网里广播本机，客户端须手动填写地址

通用选项：
  --name     显示给对方的设备名，默认取计算机名
  --no-clipboard
             不同步剪贴板。任意一端加上即关闭
  --data-dir 数据目录（本机身份与已配对设备），默认为系统的应用数据目录
  -v         输出调试日志
  -h         显示本帮助

连接全程 TLS 1.3 加密。两台电脑第一次连接时，两边终端都会显示一个 6 位配对码，
核对一致后分别输入 y 确认。

光标在对方屏幕时，按 Ctrl + Option + Esc 可以强制回到本机。";

pub(crate) enum Parsed {
    Run(Options),
    Help,
}

pub(crate) struct Options {
    pub(crate) mode: Mode,
    pub(crate) name: String,
    pub(crate) clipboard: bool,
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) verbose: bool,
}

pub(crate) enum Mode {
    /// 常驻运行，按 `config.toml` 工作。`autostart`：登录时由系统启动，不打开设置界面。
    Resident {
        autostart: bool,
        /// 内部参数：这次已经是为提升权限而重新启动的进程，不能再弹一次 UAC。
        elevation_attempted: bool,
    },
    Serve {
        listen: SocketAddr,
        side: Side,
        swap_cmd_ctrl: bool,
        announce: bool,
    },
    Connect {
        /// 不填时在局域网里寻找。
        server: Option<SocketAddr>,
    },
    Peers,
    Forget {
        target: String,
    },
    /// 请正在运行的后台发送这些文件。
    Send {
        paths: Vec<PathBuf>,
    },
    /// 请正在运行的后台退出。
    Quit,
    /// 调试：把边缘投递胶囊的各种样子画成 PNG，放进 `dir`。
    PreviewCapsule {
        dir: PathBuf,
    },
}

pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter().peekable();
    let command = match args.peek().map(String::as_str) {
        Some("-h" | "--help" | "help") => return Ok(Parsed::Help),
        // 不带命令（或直接跟选项）：常驻运行。
        None => "resident".to_owned(),
        Some(first) if first.starts_with('-') => "resident".to_owned(),
        Some(
            command @ ("serve" | "connect" | "peers" | "forget" | "send" | "quit"
            | "preview-capsule"),
        ) => {
            let command = command.to_owned();
            args.next();
            command
        }
        Some(other) => return Err(format!("未知命令：{other}")),
    };
    let runs = matches!(command.as_str(), "serve" | "connect");

    let mut listen = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    let mut side = Side::Right;
    let mut swap_cmd_ctrl = true;
    let mut announce = true;
    let mut clipboard = true;
    let mut positional = None;
    let mut paths = Vec::new();
    let mut name = None;
    let mut data_dir = None;
    let mut verbose = false;
    let mut autostart = false;
    let mut elevation_attempted = false;

    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} 缺少取值"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-v" | "--verbose" => verbose = true,
            "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--autostart" if command == "resident" => autostart = true,
            "--elevation-attempt" if command == "resident" => elevation_attempted = true,
            "--name" if runs => name = Some(value("--name")?),
            "--no-clipboard" if runs => clipboard = false,
            "--listen" if command == "serve" => listen = resolve(&value("--listen")?)?,
            "--side" if command == "serve" => side = parse_side(&value("--side")?)?,
            "--no-cmd-ctrl-swap" if command == "serve" => swap_cmd_ctrl = false,
            "--no-announce" if command == "serve" => announce = false,
            other
                if matches!(command.as_str(), "connect" | "forget" | "preview-capsule")
                    && positional.is_none()
                    && !other.starts_with('-') =>
            {
                positional = Some(other.to_owned());
            }
            other if command == "send" && !other.starts_with('-') => {
                paths.push(PathBuf::from(other))
            }
            other => return Err(format!("无法识别的参数：{other}")),
        }
    }

    let mode = match command.as_str() {
        "resident" => Mode::Resident {
            autostart,
            elevation_attempted,
        },
        "serve" => Mode::Serve {
            listen,
            side,
            swap_cmd_ctrl,
            announce,
        },
        "connect" => Mode::Connect {
            server: positional.as_deref().map(resolve).transpose()?,
        },
        "peers" => Mode::Peers,
        "quit" => Mode::Quit,
        "send" if paths.is_empty() => return Err("send 需要至少一个文件或文件夹".into()),
        "send" => Mode::Send { paths },
        "preview-capsule" => Mode::PreviewCapsule {
            dir: PathBuf::from(positional.ok_or("preview-capsule 需要一个输出目录")?),
        },
        _ => Mode::Forget {
            target: positional.ok_or("forget 需要设备名或指纹，可先用 peers 查看")?,
        },
    };
    Ok(Parsed::Run(Options {
        mode,
        name: name.unwrap_or_else(default_name),
        clipboard,
        data_dir,
        verbose,
    }))
}

/// 解析 `IP`、`IP:端口`、`主机名` 或 `主机名:端口`，省略端口时使用默认端口。
fn resolve(text: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    let with_port = if text.contains(':') {
        text.to_owned()
    } else {
        format!("{text}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .ok_or_else(|| format!("无法解析地址：{text}"))
}

fn parse_side(text: &str) -> Result<Side, String> {
    match text {
        "right" => Ok(Side::Right),
        "left" => Ok(Side::Left),
        "top" => Ok(Side::Top),
        "bottom" => Ok(Side::Bottom),
        _ => Err(format!(
            "--side 只能是 right、left、top、bottom，而不是 {text}"
        )),
    }
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "Mac".into()
            } else {
                "ScreenFerry".into()
            }
        })
}
