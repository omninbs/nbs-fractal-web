//! 双击（无控制终端）时把自己放进终端窗口重开。
//!
//! 打开终端窗口没有跨平台标准接口，各终端接收命令的参数互不兼容，
//! 因此这里按平台各自独立枚举；本模块只依赖 `std`，与 crate 其余部分无耦合。

use std::io::{self, IsTerminal};
#[cfg(unix)]
use std::process::Command;
#[cfg(all(unix, not(target_os = "macos")))]
use std::time::Duration;

/// 哨兵环境变量：子进程已在终端里运行，不再递归重开
const SENTINEL: &str = "NBS_FRACTAL_WEB_IN_TERMINAL";

/// 若当前进程不是从终端启动，就借本机终端模拟器重新运行自己并返回 `true`
/// （调用方应直接退出）；已在终端中或无法拉起终端时返回 `false`。
pub fn relaunch_in_terminal() -> bool {
    if io::stdin().is_terminal() || std::env::var_os(SENTINEL).is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };

    #[cfg(windows)]
    {
        // console 子系统由系统分配窗口，双击即有终端
        let _ = exe;
        false
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-a", "Terminal"])
            .arg(exe)
            .env(SENTINEL, "1")
            .spawn()
            .is_ok()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // 无图形会话（服务器 / systemd）不尝试开终端，照常后台服务
        let graphical = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
        if !graphical {
            return false;
        }

        /// 终端可执行名与「把其后参数当作命令」的分隔参数
        const CANDIDATES: &[(&str, &[&str])] = &[
            // XDG 提案中的跨桌面标准入口
            ("xdg-terminal-exec", &[]),
            ("x-terminal-emulator", &["-e"]),
            ("kgx", &["-e"]),
            ("gnome-terminal", &["--"]),
            ("ptyxis", &["--"]),
            ("konsole", &["-e"]),
            ("xfce4-terminal", &["-x"]),
            ("mate-terminal", &["-x"]),
            ("lxterminal", &["-e"]),
            ("tilix", &["-e"]),
            ("alacritty", &["-e"]),
            ("kitty", &[]),
            ("foot", &[]),
            ("wezterm", &["start", "--"]),
            ("urxvt", &["-e"]),
            ("xterm", &["-e"]),
        ];

        let mut commands: Vec<(String, Vec<String>)> = Vec::new();
        if let Some(terminal) = std::env::var_os("TERMINAL") {
            commands.push((
                terminal.to_string_lossy().into_owned(),
                vec!["-e".to_string()],
            ));
        }
        for (program, prefix) in CANDIDATES {
            commands.push((
                (*program).to_string(),
                prefix.iter().map(|arg| (*arg).to_string()).collect(),
            ));
        }

        for (program, prefix) in commands {
            let Ok(mut child) = Command::new(&program)
                .args(&prefix)
                .arg(&exe)
                .env(SENTINEL, "1")
                .spawn()
            else {
                continue;
            };
            // 终端可能拉起窗口后自行退出（成功），也可能因参数/显示问题秒退（失败）
            std::thread::sleep(Duration::from_millis(250));
            match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(status)) if status.success() => return true,
                _ => continue,
            }
        }
        false
    }
}

/// 出错时别让终端窗口一闪而过；无终端（守护进程）下不阻塞
pub fn wait_for_enter() {
    if io::stdin().is_terminal() {
        eprintln!("Press Enter to exit...");
        let _ = io::stdin().read_line(&mut String::new());
    }
}
