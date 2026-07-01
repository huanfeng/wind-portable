//! 与 wind_input 核心的极简 RPC 客户端。
//!
//! 复用本仓库已验证的做法（见 `wind_setting`）：命名管道客户端用 `std::fs::File`
//! （Windows 允许 `OpenOptions` 打开 `\\.\pipe\...`），帧格式为 4 字节大端长度 + JSON，
//! 故除"连接"一步外全是跨平台 `std::io`，纯逻辑可在 Linux 单测。
//!
//! 方法约定（与新 Rust 服务 `wind-rpc` 对齐）：
//!   - 存活探测 → `system.status`（无 `System.Ping`）。
//!   - 优雅关闭 → `system.shutdown`（**当前服务端未实现**，预留接口：调用失败即回退终止进程）。

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::variant::Variant;

/// 协议版本（与 wind-ipc 对齐）。
const PROTOCOL_VERSION: i32 = 1;
/// 单帧上限 16MiB。
const MAX_FRAME: u32 = 16 * 1024 * 1024;

#[derive(Serialize)]
struct RpcRequest<'a> {
    version: i32,
    id: u64,
    method: &'a str,
    params: &'a Value,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// 控制通道端点：Windows 命名管道 / Unix 套接字。
pub fn ctrl_endpoint(variant: &Variant) -> String {
    let suffix = &variant.pipe_suffix;
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\wind_input{suffix}_ctrl")
    }
    #[cfg(not(windows))]
    {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{dir}/wind_input{suffix}_ctrl.sock")
    }
}

/// 探测核心是否在运行：连接 ctrl 通道并调用 `system.status`，成功即视为运行中。
pub fn is_running(variant: &Variant) -> bool {
    call(variant, "system.status", &json!({})).is_ok()
}

/// 预留接口：请求核心优雅关闭。当前服务端未实现 `system.shutdown`，调用必返回 `Err`，
/// 调用方据此回退到终止进程。主仓库补上该方法后，本函数即自动生效（零改动联调）。
pub fn try_shutdown(variant: &Variant) -> bool {
    call(variant, "system.shutdown", &json!({})).is_ok()
}

/// 单次 RPC 调用：连接端点 → 写请求帧 → 读响应帧 → 解析。
fn call(variant: &Variant, method: &str, params: &Value) -> Result<Value, String> {
    let endpoint = ctrl_endpoint(variant);
    let mut stream = connect(&endpoint)?;
    call_on_stream(&mut stream, method, params)
}

/// 单次连接（**不重试**）：作为探活原语，管道不存在时立即失败（FILE_NOT_FOUND 不阻塞）、
/// 存在则毫秒级握手。重试会让探活在服务未起时阻塞上千毫秒——绝不能放在 UI 线程路径上；
/// 启动后的"等待就绪"由调用方自行轮询（见 ui 的启动确认循环）。
#[cfg(windows)]
fn connect(endpoint: &str) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(endpoint)
        .map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))
}

#[cfg(not(windows))]
fn connect(endpoint: &str) -> Result<std::os::unix::net::UnixStream, String> {
    std::os::unix::net::UnixStream::connect(endpoint)
        .map_err(|e| format!("连接 core 失败 ({endpoint}): {e}"))
}

/// 在已连接的字节流上完成一次请求-响应（跨平台）。
fn call_on_stream<S: Read + Write>(
    stream: &mut S,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let req = RpcRequest {
        version: PROTOCOL_VERSION,
        id: 1,
        method,
        params,
    };
    let payload = serde_json::to_vec(&req).map_err(|e| format!("序列化失败: {e}"))?;
    write_frame(stream, &payload).map_err(|e| format!("发送失败: {e}"))?;
    let resp_buf = read_frame(stream).map_err(|e| format!("读取失败: {e}"))?;
    let resp: RpcResponse =
        serde_json::from_slice(&resp_buf).map_err(|e| format!("解析响应失败: {e}"))?;
    if let Some(err) = resp.error {
        return Err(err);
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

/// 写一帧：4 字节大端长度 + 载荷。
fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// 读一帧：先读 4 字节长度，再读对应载荷。
fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("帧过大: {len} 字节"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        // 4 字节长度前缀 = 5。
        assert_eq!(&buf[..4], &5u32.to_be_bytes());
        let mut cur = Cursor::new(buf);
        let got = read_frame(&mut cur).unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn read_frame_rejects_oversize() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME + 1).to_be_bytes());
        let mut cur = Cursor::new(buf);
        assert!(read_frame(&mut cur).is_err());
    }

    /// 用一个"回环"模拟服务端：读请求帧、回写一个 result 响应帧，验证 call_on_stream。
    #[test]
    fn call_on_stream_parses_result() {
        struct Loopback {
            to_read: Cursor<Vec<u8>>,
            written: Vec<u8>,
        }
        impl Read for Loopback {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.to_read.read(b)
            }
        }
        impl Write for Loopback {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.written.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // 预置一个成功响应帧。
        let resp = json!({"id":1, "result": {"chinese": true}});
        let payload = serde_json::to_vec(&resp).unwrap();
        let mut framed = Vec::new();
        write_frame(&mut framed, &payload).unwrap();

        let mut lb = Loopback {
            to_read: Cursor::new(framed),
            written: Vec::new(),
        };
        let got = call_on_stream(&mut lb, "system.status", &json!({})).unwrap();
        assert_eq!(got, json!({"chinese": true}));
        // 请求帧已写出，含 method。
        let written = String::from_utf8_lossy(&lb.written);
        assert!(written.contains("system.status"));
        assert!(written.contains("\"version\":1"));
    }

    #[test]
    fn call_on_stream_surfaces_error() {
        let resp = json!({"id":1, "error": "boom"});
        let payload = serde_json::to_vec(&resp).unwrap();
        let mut framed = Vec::new();
        write_frame(&mut framed, &payload).unwrap();
        let mut cur = Cursor::new(framed);
        // 仅读路径：用一个只读流 + 丢弃写。
        struct R(Cursor<Vec<u8>>);
        impl Read for R {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(b)
            }
        }
        impl Write for R {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut r = R(cur.clone());
        let _ = &mut cur;
        let err = call_on_stream(&mut r, "system.status", &json!({})).unwrap_err();
        assert_eq!(err, "boom");
    }
}
