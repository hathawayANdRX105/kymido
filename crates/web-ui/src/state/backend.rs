//! 数据后端：唯一真实来源是本机 kymido daemon。

use web_client::daemon::WebDaemon;
use web_state::types::WorkspaceSpace;

/// 数据后端：唯一真实来源是本机 kymido daemon。初始化连接失败
/// （无 daemon / ping 不通）→ [`DataBackend::Disconnected`]，全部数据源
/// 返回空集合（空态），不回退假数据、不 panic。
#[derive(Debug, Clone)]
pub enum DataBackend {
    Daemon(WebDaemon),
    /// daemon 不可达：项目/会话/消息/任务一律空态。
    Disconnected,
}

/// Daemon 模式下的唯一真实项目行：名字取 data_dir 的文件名。
pub fn daemon_space(data_dir: &str) -> WorkspaceSpace {
    let name = std::path::Path::new(data_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "kymido".into());
    WorkspaceSpace {
        id: data_dir.to_string(),
        name,
        path: data_dir.to_string(),
        branch: String::new(),
        is_active: true,
    }
}

/// 探测 daemon：连接 + ping 放独立线程执行（UDS 往返通常 <10ms），主渲染
/// 线程最多等 2s（`recv_timeout`）。daemon 卡住（socket 存在但 peer 不响应）
/// 时首帧不再冻死：超时退化为 Disconnected，空态先渲染，数据由事件订阅补。
/// 保留 JoinError 日志——socket 解析或 ping 里的真 panic 不能静默吞。
pub fn probe_backend() -> DataBackend {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 内层 spawn + join：保留 JoinError 的 panic 检测语义；外层 2s
        // 有界等待：daemon 卡住时不阻塞首帧。代价是有界的：`use_signal`
        // 初始化只在挂载时跑一次，超时路径会滞留「探测线程 + relay 线程」
        // 一对，卡住的 ping 一断（daemon 恢复 / socket 关闭）二者自然退出，
        // 非永久泄漏。
        let _ = tx.send(
            std::thread::spawn(|| WebDaemon::from_env_or_default().filter(|d| d.ping())).join(),
        );
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(Some(d))) => DataBackend::Daemon(d),
        Ok(Ok(None)) => DataBackend::Disconnected,
        Ok(Err(e)) => {
            eprintln!("[web] workspace daemon probe thread panicked: {e:?}");
            DataBackend::Disconnected
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("[web] daemon probe timed out (>2s); rendering disconnected state");
            DataBackend::Disconnected
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => DataBackend::Disconnected,
    }
}
