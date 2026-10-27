// 设置：目前只有多设备写入开关。开关只改刻录页与追加页的表单形态（单设备或
// 设备配对表），写盘行为与后端分派不受影响（ADR-0025 补记）。
import { t } from "../i18n";
import { FormRow } from "../components/FormRow";

type SettingsPageProps = {
  multiDevice: boolean;
  onMultiDeviceChange: (enabled: boolean) => void;
};

export function SettingsPage({ multiDevice, onMultiDeviceChange }: SettingsPageProps) {
  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Settings", "设置")}</h1>
          <p>{t("Adjust how the burn and append pages look.", "调整刻录页与追加页的形态。")}</p>
        </div>
      </header>
      <div className="form">
        <FormRow
          label={t("Multi-device writing", "多设备写入")}
          hint={t(
            "When enabled, the burn and append pages pair content with several drives and submit them at once. When disabled, they stay a single-device form.",
            "开启后刻录页与追加页用设备与内容配对的表单，可一次向多台光驱提交；关闭时保持单设备表单。",
          )}
        >
          <label className="option">
            <input
              type="checkbox"
              checked={multiDevice}
              onChange={(event) => onMultiDeviceChange(event.target.checked)}
            />
            {t("Enable multi-device writing", "启用多设备写入")}
          </label>
        </FormRow>
      </div>
    </div>
  );
}
