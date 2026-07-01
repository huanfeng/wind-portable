//! 命令行选项解析（对应 C# `Program.ParseCLI`）。

/// 解析后的命令行动作。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CliOptions {
    pub start: bool,
    pub stop: bool,
    pub status: bool,
    pub settings: bool,
    pub userdata: bool,
    /// 显式要求 UI（即使带了其他动作也走界面）。
    pub ui: bool,
    /// 内部：已提权后直接注册（regsvr32 + InstallLayoutOrTip）。
    pub elevate_register: bool,
    /// 内部：已提权后直接注销。
    pub elevate_unregister: bool,
}

impl CliOptions {
    /// 是否带了需在无界面下直接执行的动作。
    pub fn has_action(&self) -> bool {
        self.start
            || self.stop
            || self.status
            || self.settings
            || self.userdata
            || self.elevate_register
            || self.elevate_unregister
    }
}

/// 解析参数（接受 `-x` 与 `--x` 两种写法，大小写不敏感）。
pub fn parse(args: &[String]) -> CliOptions {
    let mut o = CliOptions::default();
    for arg in args {
        match arg.to_ascii_lowercase().as_str() {
            "-start" | "--start" => o.start = true,
            "-stop" | "--stop" => o.stop = true,
            "-status" | "--status" => o.status = true,
            "-settings" | "--settings" => o.settings = true,
            "-userdata" | "--userdata" => o.userdata = true,
            "-ui" | "--ui" => o.ui = true,
            "-elevate-register" => o.elevate_register = true,
            "-elevate-unregister" => o.elevate_unregister = true,
            _ => {}
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> CliOptions {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn empty_has_no_action() {
        let o = p(&[]);
        assert!(!o.has_action());
        assert!(!o.ui);
    }

    #[test]
    fn parses_each_flag_both_dashes() {
        assert!(p(&["-start"]).start);
        assert!(p(&["--start"]).start);
        assert!(p(&["-STOP"]).stop);
        assert!(p(&["--status"]).status);
        assert!(p(&["-settings"]).settings);
        assert!(p(&["-userdata"]).userdata);
        assert!(p(&["-ui"]).ui);
        assert!(p(&["-elevate-register"]).elevate_register);
        assert!(p(&["-elevate-unregister"]).elevate_unregister);
    }

    #[test]
    fn has_action_covers_actions_not_ui() {
        assert!(p(&["-start"]).has_action());
        assert!(p(&["-elevate-register"]).has_action());
        assert!(!p(&["-ui"]).has_action());
    }

    #[test]
    fn ignores_unknown() {
        let o = p(&["--frobnicate", "-start", "junk"]);
        assert!(o.start);
        assert!(!o.stop);
    }
}
