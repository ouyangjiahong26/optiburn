// 默认落地页：挂载时探测一次，job-done 后留在本页时由 App 发信号自动重探。
import { useEffect } from "react";
import { RefreshCw } from "lucide-react";
import { useDeviceProbe } from "../hooks";
import { StatusBadge } from "../components/StatusBadge";

export function DevicesPage({
  locked,
  refreshSignal,
}: {
  locked: boolean;
  refreshSignal: number;
}) {
  const { devices, probing, error, refresh } = useDeviceProbe();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (refreshSignal > 0) {
      void refresh();
    }
  }, [refreshSignal, refresh]);

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>设备</h1>
          <p>查看本机光驱与盘片状态。</p>
        </div>
        <button
          type="button"
          className="btn"
          disabled={locked || probing}
          onClick={() => void refresh()}
        >
          <RefreshCw size={13} className={probing ? "spin" : undefined} />
          刷新
        </button>
      </header>
      {error !== null && (
        <div className="result-banner warn">探测设备失败：{error}</div>
      )}
      {devices === null ? (
        error === null && <p className="page-note">正在探测设备……</p>
      ) : devices.length === 0 ? (
        <div className="empty-state">未发现光驱</div>
      ) : (
        <ul className="device-list">
          {devices.map((device) => (
            <li key={device.path} className="device-card">
              <div className="device-main">
                <span className="device-path">{device.path}</span>
                {device.status !== null && <StatusBadge status={device.status} />}
              </div>
              <div className="device-meta">
                <span>{device.identity ?? "未知型号"}</span>
                {device.sessions !== null && (
                  <span>{device.sessions} 个区段</span>
                )}
              </div>
              {device.error !== null && (
                <p className="device-error">{device.error}</p>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
