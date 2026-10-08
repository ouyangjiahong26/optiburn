// 输入框 + 浏览按钮：目录选择、保存 iso、打开 iso 三种模式由 mode 决定。
import { open, save } from "@tauri-apps/plugin-dialog";

export type PathFieldMode = "directory" | "saveIso" | "openIso";

const ISO_FILTER = { name: "镜像文件", extensions: ["iso"] };

export function PathField({
  mode,
  value,
  onChange,
  disabled,
  placeholder,
}: {
  mode: PathFieldMode;
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
  placeholder?: string;
}) {
  async function browse() {
    if (mode === "saveIso") {
      const picked = await save({ filters: [ISO_FILTER] });
      if (picked !== null) {
        onChange(picked);
      }
      return;
    }
    const picked = await open(
      mode === "directory"
        ? { directory: true, multiple: false }
        : { multiple: false, filters: [ISO_FILTER] },
    );
    // Windows 侧的实现可能返回数组，这里只接受单个字符串结果。
    if (typeof picked === "string") {
      onChange(picked);
    }
  }

  return (
    <div className="path-field">
      <input
        type="text"
        className="text-input"
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      <button
        type="button"
        className="btn"
        disabled={disabled}
        onClick={() => void browse()}
      >
        浏览
      </button>
    </div>
  );
}
